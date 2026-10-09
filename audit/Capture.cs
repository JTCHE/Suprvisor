using System.Diagnostics;
using System.Runtime.InteropServices;
using Vortice.Direct3D;
using Vortice.Direct3D11;
using Vortice.DXGI;
using Vortice.Mathematics;

// One captured screen frame inside the watched rectangle.
// Time is the DWM present time (QPC ticks). Changed = fraction of pixels that differ
// from the previous frame; FromStart = fraction that differs from the first frame.
// Thumb = every 4th pixel on both axes, kept for changed frames only (to compare with the settled image later).
public record Frame(long Time, double Changed, double FromStart, int Accumulated, uint[]? Thumb);

// Desktop Duplication of the monitor that holds `rect` (physical pixels, desktop coordinates).
// Records every present that touches the screen, with the change fraction inside `rect`.
// Same integrity level not needed: it reads screen pixels, so it works for elevated windows too.
public sealed class Capture : IDisposable
{
    readonly ID3D11Device _device;
    readonly ID3D11DeviceContext _ctx;
    readonly IDXGIOutputDuplication _dup;
    readonly ID3D11Texture2D _staging;
    readonly Box _box;
    readonly int _w, _h;
    readonly Thread _thread;
    volatile bool _stop;
    public readonly List<Frame> Frames = new();

    public Capture(Rect rect)
    {
        using var factory = DXGI.CreateDXGIFactory1<IDXGIFactory1>();
        for (uint a = 0; factory.EnumAdapters1(a, out var adapter).Success; a++)
        for (uint o = 0; adapter.EnumOutputs(o, out var output).Success; o++)
        {
            var d = output.Description.DesktopCoordinates;
            if (rect.Left < d.Left || rect.Top < d.Top || rect.Right > d.Right || rect.Bottom > d.Bottom) { output.Dispose(); continue; }
            D3D11.D3D11CreateDevice(adapter, DriverType.Unknown, DeviceCreationFlags.BgraSupport, null!, out _device!, out _ctx!).CheckError();
            using (var dxgiDevice = _device!.QueryInterface<IDXGIDevice>())
                dxgiDevice.SetGPUThreadPriority(7); // best effort: keep capture ahead of the GPU load
            using var output1 = output.QueryInterface<IDXGIOutput1>();
            _dup = output1.DuplicateOutput(_device);
            output.Dispose();
            _w = rect.Right - rect.Left; _h = rect.Bottom - rect.Top;
            _box = new Box(rect.Left - d.Left, rect.Top - d.Top, 0, rect.Right - d.Left, rect.Bottom - d.Top, 1);
            _staging = _device.CreateTexture2D(new Texture2DDescription(Format.B8G8R8A8_UNorm, (uint)_w, (uint)_h, 1, 1,
                BindFlags.None, ResourceUsage.Staging, CpuAccessFlags.Read));
            _thread = new Thread(Loop) { Priority = ThreadPriority.Highest, IsBackground = true };
            _thread.Start();
            return;
        }
        throw new InvalidOperationException($"no single monitor contains {rect}");
    }

    unsafe void Loop()
    {
        var prev = new uint[_w * _h];
        var cur = new uint[_w * _h];
        uint[]? first = null;
        while (!_stop)
        {
            var r = _dup.AcquireNextFrame(50, out var info, out var res);
            if (r.Code == Vortice.DXGI.ResultCode.WaitTimeout.Code) continue;
            r.CheckError();
            try
            {
                if (info.LastPresentTime == 0 && first != null) continue; // pointer-only update
                using (res) using (var tex = res.QueryInterface<ID3D11Texture2D>())
                    _ctx.CopySubresourceRegion(_staging, 0, 0, 0, 0, tex, 0, _box);
                var map = _ctx.Map(_staging, 0, MapMode.Read);
                for (int y = 0; y < _h; y++)
                    new ReadOnlySpan<uint>((byte*)map.DataPointer + y * map.RowPitch, _w).CopyTo(cur.AsSpan(y * _w, _w));
                _ctx.Unmap(_staging, 0);
                // The first acquire returns the current screen: keep it as the reference image.
                if (first == null) { first = (uint[])cur.Clone(); (prev, cur) = (cur, prev); continue; }
                double changed = Diff(prev, cur);
                lock (Frames) Frames.Add(new Frame(info.LastPresentTime, changed, Diff(first, cur), (int)info.AccumulatedFrames, changed > 0 ? Thumb(cur) : null));
                (prev, cur) = (cur, prev);
            }
            finally { _dup.ReleaseFrame(); }
        }
    }

    uint[] Thumb(uint[] img)
    {
        var t = new uint[(_w / 4) * (_h / 4)];
        for (int y = 0, i = 0; y < _h / 4; y++) for (int x = 0; x < _w / 4; x++) t[i++] = img[y * 4 * _w + x * 4];
        return t;
    }

    public static double Diff(uint[] a, uint[] b)
    {
        int n = 0;
        for (int i = 0; i < a.Length; i++) if (a[i] != b[i]) n++;
        return (double)n / a.Length;
    }

    public List<Frame> Snapshot() { lock (Frames) return Frames.ToList(); }

    public List<Frame> Stop() { _stop = true; _thread.Join(); return Frames; }

    public void Dispose() { _stop = true; _thread.Join(); _staging.Dispose(); _dup.Dispose(); _ctx.Dispose(); _device.Dispose(); }

    public static double Ms(long qpcTicks) => qpcTicks * 1000.0 / Stopwatch.Frequency;
}

// Physical-pixel rectangle in desktop coordinates. Layout matches Win32 RECT.
[StructLayout(LayoutKind.Sequential)]
public record struct Rect(int Left, int Top, int Right, int Bottom);
