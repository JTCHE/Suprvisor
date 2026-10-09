using Vortice.D3DCompiler;
using Vortice.Direct3D;
using Vortice.Direct3D11;
using Vortice.DXGI;

// Synthetic "Houdini is cooking" load: all logical CPUs busy at normal priority,
// RAM pressure, a GPU compute loop, and VRAM filled to a fraction of dedicated memory.
// Everything runs inside this process, so it stops when the harness exits.
public sealed class Load : IDisposable
{
    volatile bool _stop;
    readonly List<Thread> _threads = new();
    readonly List<byte[]> _ram = new();
    readonly List<ID3D11Texture2D> _vram = new();
    ID3D11Device? _device;
    ID3D11DeviceContext? _ctx;

    public long RamBytes => _ram.Sum(b => (long)b.Length);
    public long VramBytes { get; private set; }

    public Load(long ramBytes, double vramFraction)
    {
        for (int i = 0; i < Environment.ProcessorCount; i++)
            Start(() => { double x = i; while (!_stop) for (int k = 0; k < 100_000; k++) x = Math.Sin(x) + 1.0001; });

        const int chunk = 256 << 20;
        for (long done = 0; done < ramBytes; done += chunk)
        {
            var b = GC.AllocateUninitializedArray<byte>(chunk);
            for (int p = 0; p < b.Length; p += 4096) b[p] = 1; // commit every page
            _ram.Add(b);
        }

        D3D11.D3D11CreateDevice(null, DriverType.Hardware, DeviceCreationFlags.None, null!, out _device!, out _ctx!).CheckError();
        using (var dxgiDevice = _device!.QueryInterface<IDXGIDevice>())
        using (var adapter = dxgiDevice.GetAdapter())
        {
            long target = (long)(adapter.Description.DedicatedVideoMemory * vramFraction);
            var desc = new Texture2DDescription(Format.R8G8B8A8_UNorm, 4096, 4096, 1, 1, BindFlags.RenderTarget);
            while (VramBytes < target)
            {
                var t = _device.CreateTexture2D(desc);
                using (var rtv = _device.CreateRenderTargetView(t)) _ctx!.ClearRenderTargetView(rtv, new Vortice.Mathematics.Color4(0.5f)); // make it resident
                _vram.Add(t);
                VramBytes += 4096L * 4096 * 4;
            }
        }

        const string hlsl = """
            RWStructuredBuffer<float> o : register(u0);
            [numthreads(256,1,1)] void main(uint3 id : SV_DispatchThreadID) {
                float x = id.x;
                [loop] for (int i = 0; i < 4000; i++) x = sin(x) * 1.0001 + cos(x);
                o[id.x & 1023] = x;
            }
            """;
        var blob = Compiler.Compile(hlsl, "main", "load.hlsl", "cs_5_0");
        var cs = _device.CreateComputeShader(blob.Span);
        var buf = _device.CreateBuffer(new BufferDescription(1024 * 4, BindFlags.UnorderedAccess, ResourceUsage.Default,
            CpuAccessFlags.None, ResourceOptionFlags.BufferStructured, 4));
        var uav = _device.CreateUnorderedAccessView(buf);
        var query = _device.CreateQuery(new QueryDescription(QueryType.Event));
        Start(() =>
        {
            _ctx!.CSSetShader(cs);
            _ctx.CSSetUnorderedAccessView(0, uav);
            while (!_stop)
            {
                for (int i = 0; i < 4; i++) _ctx.Dispatch(256, 1, 1); // short dispatches: no TDR, DWM can interleave
                _ctx.End(query);
                while (!_stop && !_ctx.IsDataAvailable(query)) Thread.Yield(); // keep at most one batch queued
            }
        });
    }

    void Start(Action body)
    {
        var t = new Thread(() => body()) { IsBackground = true };
        t.Start();
        _threads.Add(t);
    }

    public void Dispose()
    {
        _stop = true;
        foreach (var t in _threads) t.Join();
        foreach (var t in _vram) t.Dispose();
        _ctx?.Dispose();
        _device?.Dispose();
        _ram.Clear();
    }
}
