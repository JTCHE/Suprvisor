// Perf audit harness. Measures what the user sees on screen (Desktop Duplication),
// so the same numbers apply to the stock Task Manager and to Suprvisor.
// Taskmgr: must run elevated. It always restarts itself elevated, and UIPI blocks input,
// cursor moves and kills from a lower integrity level. Use audit/run.ps1 (one UAC prompt).
// Suprvisor: the resident process is shown by running suprvisor.exe (the IFEO stub path) and
// hidden with --hide, so "launch" is show-from-tray. Runs at the target's integrity level.
//   Audit.dll [--target taskmgr|suprvisor] [--exe path] [--only idle|load] [--runs N] [--sort-at x,y]
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text.Json;
using System.Text.Json.Serialization;
using static Native;

string Arg(string name) => args.SkipWhile(a => a != name).Skip(1).FirstOrDefault();
var target = Arg("--target") ?? "taskmgr";
bool tm = target == "taskmgr";
if (!tm && target != "suprvisor") { Console.Error.WriteLine($"unknown target {target}"); return 2; }
var (windowClass, processName) = tm ? ("TaskManagerWindow", "Taskmgr") : ("Suprvisor", "suprvisor");
var exe = Path.GetFullPath(Arg("--exe") ?? Path.Combine(AppContext.BaseDirectory, "../../../../target/release/suprvisor.exe"));
// Taskmgr is still loading rows and icons after 18 s. Suprvisor shows its last snapshot, then
// gets a new list ~50 ms later.
int SettleMs = tm ? 25_000 : 5_000;

if (tm && !new WindowsPrincipal(WindowsIdentity.GetCurrent()).IsInRole(WindowsBuiltInRole.Administrator))
{
    Console.Error.WriteLine("Run elevated (audit/run.ps1). Taskmgr self-elevates; UIPI blocks a non-elevated harness.");
    return 2;
}
SetProcessDpiAwarenessContext(-4); // per-monitor v2: all rects in physical pixels
var only = Arg("--only");
int runs = args.SkipWhile(a => a != "--runs").Skip(1).Select(int.Parse).FirstOrDefault(5);
// Name column header, in physical px from the window's top-left. Each click reverses the whole list
// (A-Z / Z-A), so every click gives a large, detectable change. Taskmgr's XAML is not visible to UIA,
// so this is a fixed point. Default = 100% DPI, stock layout. Suprvisor's point comes from its layout.
var sortAt = args.SkipWhile(a => a != "--sort-at").Skip(1).Select(a => a.Split(',').Select(int.Parse).ToArray()).FirstOrDefault() ?? (tm ? [290, 130] : null);
var resultsDir = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../results"));
Directory.CreateDirectory(resultsDir);

foreach (var withLoad in new[] { false, true }.Where(l => only == null || only == (l ? "load" : "idle")))
{
    var result = Suite(withLoad);
    var file = Path.Combine(resultsDir, $"{DateTime.Now:yyyyMMdd-HHmmss}-{target}-{(withLoad ? "load" : "idle")}.json");
    File.WriteAllText(file, JsonSerializer.Serialize(result, new JsonSerializerOptions
        { WriteIndented = true, NumberHandling = JsonNumberHandling.AllowNamedFloatingPointLiterals }));
    Console.WriteLine($"wrote {file}");
}
return 0;

object Suite(bool withLoad)
{
    Console.WriteLine($"== {target}, load {withLoad}, {runs} launches");
    using var load = withLoad ? new Load(ramBytes: 16L << 30, vramFraction: 0.7) : null;
    if (load != null) { Console.WriteLine($"load: {Environment.ProcessorCount} CPU threads, {load.RamBytes >> 20} MB RAM, {load.VramBytes >> 20} MB VRAM"); Thread.Sleep(3000); }

    // Warm-up launch: loads binaries into the file cache and gives the window rectangle.
    Kill();
    Launch();
    var rect = WindowRect(WaitWindow());
    Thread.Sleep(3000);
    rect = WindowRect(WaitWindow());
    Console.WriteLine($"window rect {rect.Left},{rect.Top} {rect.Right - rect.Left}x{rect.Bottom - rect.Top}");
    Kill();
    // Static backdrop under the target, so only the target's own pixels change inside the rect.
    using var curtain = new Curtain(rect);

    // Launch: hotkey to first paint, and to VC90 (visually complete: within 10% of the settled image).
    var launches = new List<LaunchRun>();
    List<Frame>? sample = null;
    for (int run = 0; run < runs; run++)
    {
        Thread.Sleep(1000);
        if (Visible()) throw new Exception("target window still open before launch");
        curtain.Activate();
        Thread.Sleep(300);
        using var cap = new Capture(rect);
        Thread.Sleep(300); // capture holds the reference image before t0
        long t0 = Stopwatch.GetTimestamp();
        Launch();
        Thread.Sleep(SettleMs);
        var frames = cap.Stop().Where(f => f.Time >= t0).ToList();
        var thumbs = frames.Where(f => f.Thumb != null).ToList();
        var final = thumbs[^1].Thumb!;
        int lastFar = thumbs.FindLastIndex(f => Capture.Diff(f.Thumb!, final) > 0.10);
        // The window appearing changes most of the rect from one frame to the next.
        var firstPaint = frames.FirstOrDefault(f => f.Changed > 0.02);
        var vc90 = lastFar + 1 < thumbs.Count ? thumbs[lastFar + 1] : null;
        double Rel(Frame? f) => f == null ? double.NaN : Capture.Ms(f.Time - t0);
        launches.Add(new LaunchRun(Rel(firstPaint), Rel(vc90), frames.Count, frames.Sum(f => f.Accumulated - 1),
            WindowRect(FindWindow(windowClass, null)).Equals(rect)));
        Console.WriteLine($"launch {run}: first paint {Rel(firstPaint):F0} ms, VC90 {Rel(vc90):F0} ms");
        Console.WriteLine("  frames: " + string.Join(" ", frames.Where(f => f.Changed > 0).Take(8).Select(f => $"{Rel(f):F0}ms:{f.Changed:F2}/{f.FromStart:F2}")));
        sample ??= frames;
        if (run < runs - 1) Kill();
    }

    // Sort: click the Name header (reverses the list), time to the first frame that changes >2% of the
    // Name column's rows. Only a reorder changes names; live numbers (other columns) never do.
    Click(new System.Drawing.Point(rect.Left + 250, rect.Top + 20)); // focus via title bar: Mica changes on activation
    Thread.Sleep(1000);
    var header = sortAt != null ? new System.Drawing.Point(rect.Left + sortAt[0], rect.Top + sortAt[1]) : NameHeader();
    var sorts = new List<double>();
    var names = new Rect(rect.Left, header.Y + (header.Y - rect.Top) / 2, rect.Left + (rect.Right - rect.Left) * 2 / 5, rect.Bottom);
    using (var cap = new Capture(names))
    {
        Thread.Sleep(300);
        for (int i = 0; i < 10; i++)
        {
            long t0 = Click(header);
            Frame? hit = null; // taskmgr presents nothing while it sorts, so wait for the frame, then settle
            for (var w = Stopwatch.StartNew(); hit == null && w.ElapsedMilliseconds < 5000; Thread.Sleep(20))
                hit = cap.Snapshot().FirstOrDefault(f => f.Time >= t0 && f.Changed > 0.02);
            sorts.Add(hit == null ? double.NaN : Capture.Ms(hit.Time - t0));
            if (Environment.GetEnvironmentVariable("AUDIT_QPC") != null) Console.WriteLine($"  qpc up {t0} frame {hit?.Time}");
            Console.WriteLine("  after click: " + string.Join(" ", cap.Snapshot().Where(f => f.Time >= t0).Take(4).Select(f => $"{Capture.Ms(f.Time - t0):F0}ms:{f.Changed:F3}")));
            Thread.Sleep(1500);
        }
    }
    Console.WriteLine($"sort ms: {string.Join(", ", sorts.Select(s => s.ToString("F0")))}");

    // Scroll: 2 s of wheel input every 10 ms over the list, then frame intervals. The direction flips
    // every 10 notches (30 lines), so the list never hits an end, where frames stop by design.
    List<double> intervals;
    using (var cap = new Capture(rect))
    {
        Thread.Sleep(300);
        SetCursorPos((rect.Left + rect.Right) / 2, (rect.Top + rect.Bottom) / 2 + 100);
        long s0 = Stopwatch.GetTimestamp();
        for (int i = 0; i < 200; i++) { Wheel(i / 10 % 2 == 0 ? -120 : 120); Thread.Sleep(10); }
        long s1 = Stopwatch.GetTimestamp();
        Thread.Sleep(300);
        var fr = cap.Stop().Where(f => f.Time >= s0 && f.Time <= s1 && f.Changed > 0.01).Select(f => f.Time).ToList();
        intervals = fr.Zip(fr.Skip(1), (a, b) => Capture.Ms(b - a)).ToList();
    }
    Console.WriteLine($"scroll: {intervals.Count + 1} frames in 2 s, interval p50 {Pct(intervals, 50):F1} ms, p99 {Pct(intervals, 99):F1} ms");

    // Visible cost: 10 s with the window open and untouched.
    var proc = Process.GetProcessesByName(processName).Single();
    var cpu0 = proc.TotalProcessorTime;
    Thread.Sleep(10_000);
    proc.Refresh();
    double cpuPct = (proc.TotalProcessorTime - cpu0).TotalMilliseconds / 10_000 * 100;
    Console.WriteLine($"visible: {cpuPct:F1} % of one core, private {proc.PrivateMemorySize64 >> 20} MB, working set {proc.WorkingSet64 >> 20} MB");
    Kill();

    return new
    {
        target, launchMode = tm ? "hotkey" : "exe (IFEO stub path)", load = withLoad, date = DateTime.Now,
        launch = new { firstPaintMs = Median(launches.Select(l => l.FirstPaintMs)), vc90Ms = Median(launches.Select(l => l.Vc90Ms)), runs = launches },
        sort = new { medianMs = Median(sorts), maxMs = sorts.Where(s => !double.IsNaN(s)).DefaultIfEmpty(double.NaN).Max(), missed = sorts.Count(double.IsNaN), runsMs = sorts },
        scroll = new { frames = intervals.Count + 1, p50Ms = Pct(intervals, 50), p99Ms = Pct(intervals, 99) },
        visible = new { cpuPctOneCore = cpuPct, privateMB = proc.PrivateMemorySize64 >> 20, workingSetMB = proc.WorkingSet64 >> 20 },
        sampleLaunchFrames = sample!.Where(f => f.Changed > 0).Select(f => new { ms = Math.Round(Capture.Ms(f.Time - sample[0].Time), 1), changed = Math.Round(f.Changed, 3), fromStart = Math.Round(f.FromStart, 3) }),
    };
}

// Taskmgr exits; the resident Suprvisor hides.
void Kill()
{
    if (!tm) { Process.Start(exe, "--hide").WaitForExit(); Thread.Sleep(200); return; }
    foreach (var p in Process.GetProcessesByName("Taskmgr")) { p.Kill(); p.WaitForExit(); }
}

// Ctrl+Shift+Esc, the real path. Suprvisor: what the IFEO stub will do (start the exe, which signals).
void Launch()
{
    if (tm) Keys(0x11, 0x10, 0x1B);
    else Process.Start(exe);
}

bool Visible() => FindWindow(windowClass, null) is var h && h != 0 && Shown(h);

nint WaitWindow()
{
    var sw = Stopwatch.StartNew();
    for (nint h; sw.ElapsedMilliseconds < 10_000; Thread.Sleep(5))
        if ((h = FindWindow(windowClass, null)) != 0 && Shown(h)) return h;
    throw new TimeoutException($"{target} window did not appear");
}

// Suprvisor's Name column header: client (40, 80) DIPs, see render.rs (PAD, TOP + HEAD).
System.Drawing.Point NameHeader()
{
    var h = FindWindow(windowClass, null);
    var p = new System.Drawing.Point(0, 0);
    ClientToScreen(h, ref p);
    double s = GetDpiForWindow(h) / 96.0;
    return new System.Drawing.Point(p.X + (int)(40 * s), p.Y + (int)(80 * s));
}

static double Median(IEnumerable<double> v) => Pct(v.ToList(), 50);
static double Pct(List<double> v, double p)
{
    var s = v.Where(x => !double.IsNaN(x)).Order().ToList();
    return s.Count == 0 ? double.NaN : s[(int)Math.Min(s.Count - 1, Math.Round(p / 100 * (s.Count - 1)))];
}

static class Native
{
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern nint FindWindow(string? cls, string? title);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(nint ctx);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(nint h);
    [DllImport("dwmapi.dll", EntryPoint = "DwmGetWindowAttribute")] static extern int DwmGetWindowAttributeInt(nint h, int attr, out int v, int size);
    // Visible and not cloaked (Suprvisor hides by cloaking).
    public static bool Shown(nint h) => IsWindowVisible(h) && DwmGetWindowAttributeInt(h, 14 /* DWMWA_CLOAKED */, out var c, 4) == 0 && c == 0;
    [DllImport("user32.dll")] public static extern bool ClientToScreen(nint h, ref System.Drawing.Point p);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(nint h);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(nint h, int attr, out Rect r, int size);
    [DllImport("user32.dll", SetLastError = true)] static extern uint SendInput(uint n, Input[] inputs, int size);

    [StructLayout(LayoutKind.Sequential)] struct MouseInput { public int dx, dy; public int data; public uint flags, time; public nint extra; }
    [StructLayout(LayoutKind.Sequential)] struct KeybdInput { public ushort vk, scan; public uint flags, time; public nint extra; }
    [StructLayout(LayoutKind.Explicit)] struct Input
    {
        [FieldOffset(0)] public uint type;
        [FieldOffset(8)] public MouseInput mi;
        [FieldOffset(8)] public KeybdInput ki;
    }

    public static Rect WindowRect(nint h)
    {
        DwmGetWindowAttribute(h, 9 /* DWMWA_EXTENDED_FRAME_BOUNDS */, out var r, Marshal.SizeOf<Rect>());
        return r;
    }

    static void Send(params Input[] i)
    {
        if (SendInput((uint)i.Length, i, Marshal.SizeOf<Input>()) != i.Length)
            throw new Exception($"SendInput blocked, error {Marshal.GetLastPInvokeError()}");
    }

    public static void Keys(params ushort[] vks)
    {
        var down = vks.Select(v => new Input { type = 1, ki = new KeybdInput { vk = v } });
        var up = vks.Reverse().Select(v => new Input { type = 1, ki = new KeybdInput { vk = v, flags = 2 } });
        Send(down.Concat(up).ToArray());
    }

    // Returns the QPC time of the button-up, which is when a column header sorts.
    public static long Click(System.Drawing.Point p)
    {
        if (!SetCursorPos(p.X, p.Y)) throw new Exception("SetCursorPos failed (higher-integrity window in front?)");
        Send(new Input { type = 0, mi = new MouseInput { flags = 0x2 } });
        Thread.Sleep(30);
        long t = Stopwatch.GetTimestamp();
        Send(new Input { type = 0, mi = new MouseInput { flags = 0x4 } });
        return t;
    }

    public static void Wheel(int delta) => Send(new Input { type = 0, mi = new MouseInput { data = delta, flags = 0x800 } });
}

record LaunchRun(double FirstPaintMs, double Vc90Ms, int Frames, int MissedFrames, bool SameRect);
