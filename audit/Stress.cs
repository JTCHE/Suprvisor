// Stress test: an impatient user at machine speed. Posts input messages straight to Suprvisor's
// window (no real mouse or keyboard, so it runs next to other work). The window starts without
// focus (--inactive) and is parked topmost with a 2x2 px corner on screen (top-left): the compositor takes
// frames only from a window it can show, and a covered window would count as the app stalling.
// Optionally with process churn
// and all CPUs busy, then reads the app's timing trace (SUPRVISOR_TRACE, crates/suprvisor-app/src/trace.rs).
//   Audit.dll stress [--exe path] [--seconds N] [--only sort,wheel,...] [--churn N] [--load]
// Per scenario: handler and paint times, the longest gap between frames, and input-to-frame latency
// (post time to the end of the first Present that starts after the handler began). Latency that
// grows during a scenario means the UI thread falls behind its input.
using System.Diagnostics;
using System.Runtime.InteropServices;

public static class Stress
{
    const uint WM_SIZE = 0x0005, WM_KEYDOWN = 0x0100, WM_CHAR = 0x0102, WM_MOUSEMOVE = 0x0200,
        WM_LBUTTONDOWN = 0x0201, WM_LBUTTONUP = 0x0202, WM_MOUSEWHEEL = 0x020A;
    // Layout in DIPs, from render.rs.
    const double TITLE = 48, TOP = 100, HEAD = 52, ROW = 28, PAD = 12, BAR = 12, NAV = 48;
    static readonly double[] Widths = { 72, 96, 64, 96, 128, 64 }; // settings.rs defaults

    record Post(long Us, string Event);

    public static int Run(string[] args)
    {
        string? Arg(string name) => args.SkipWhile(a => a != name).Skip(1).FirstOrDefault();
        var exe = Path.GetFullPath(Arg("--exe") ?? Path.Combine(AppContext.BaseDirectory, "../../../../target/release/suprvisor.exe"));
        int seconds = int.Parse(Arg("--seconds") ?? "6");
        int churn = int.Parse(Arg("--churn") ?? "0");
        var only = Arg("--only")?.Split(',');
        timeBeginPeriod(1);
        SetProcessDpiAwarenessContext(-4);

        // A test instance (SUPRVISOR_TEST): its own window class and settings file, so the user's
        // Suprvisor keeps running and keeps its settings. Known view state: default columns,
        // Processes page, closed navigation pane.
        Environment.SetEnvironmentVariable("SUPRVISOR_TEST", "1"); // inherited by every launch below
        var ini = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "suprvisor-test", "settings.ini");
        var scenarios = new (string Name, Action<Ctx> Act)[]
        {
            ("sort", Sort), ("wheel", Wheel), ("thumb", Thumb), ("hover", Hover), ("type", Type),
            ("keys", Keys), ("tabs", Tabs), ("columns", Columns), ("resize", Resize), ("everything", Everything),
        };
        using var load = args.Contains("--load") ? new Load(0, 0) : null;
        using var churner = churn > 0 ? new Churn(churn) : null;
        if (load != null || churner != null) Thread.Sleep(3000);
        Console.WriteLine($"stress: {seconds} s per scenario, churn {churn} processes, CPU load {(load != null ? "all cores" : "none")}");
        Console.WriteLine($"{"scenario",-11}{"inputs",7}{"handler p99/max",17}{"draw p50/p99",14}{"frame gap max",15}{"latency p50/p99/max ms",24}{"behind",8}");
        {
            foreach (var (name, act) in scenarios.Where(s => only == null || only.Contains(s.Name)))
            {
                Directory.CreateDirectory(Path.GetDirectoryName(ini)!);
                var (px, py) = Parked(1200, 800);
                File.WriteAllText(ini, $"sort=cpu\ndesc=1\ngrouped=1\ntab=processes\nnav=closed\nwindow={px},{py},{px + 1200},{py + 800},0\n");
                var trace = Path.Combine(Path.GetTempPath(), $"suprvisor-trace-{name}.txt");
                File.Delete(trace);
                Process.Start(exe, "--exit").WaitForExit();
                Thread.Sleep(300);
                var psi = new ProcessStartInfo(exe, "--inactive") { UseShellExecute = false };
                psi.Environment["SUPRVISOR_TRACE"] = trace;
                var app = Process.Start(psi)!;
                nint h = 0;
                // This process's window: under load the previous instance can still be closing.
                for (var w = Stopwatch.StartNew(); h == 0 && w.ElapsedMilliseconds < 10_000; Thread.Sleep(10))
                    for (nint c = FindWindowEx(0, 0, "SuprvisorTest", null); c != 0 && h == 0; c = FindWindowEx(0, c, "SuprvisorTest", null))
                        if (GetWindowThreadProcessId(c, out var pid) != 0 && pid == app.Id) h = c;
                if (h == 0) throw new Exception("no Suprvisor window");
                Park(h, 1200, 800);
                Thread.Sleep(4000); // full list, icons, rates
                var ctx = new Ctx(h, seconds);
                act(ctx);
                Thread.Sleep(1500);
                Process.Start(exe, "--exit").WaitForExit();
                app.WaitForExit(10_000);
                Report(name, ctx, trace);
            }
        }
        return 0;
    }

    sealed class Ctx
    {
        public readonly nint H;
        public readonly double Scale;
        public readonly int Seconds;
        public readonly List<Post> Posts = new();
        public readonly Random Rng = new(1);
        public Ctx(nint h, int seconds) { H = h; Seconds = seconds; Scale = GetDpiForWindow(h) / 96.0; }
        public (double W, double H) Size() { GetClientRect(H, out var r); return (r.R / Scale, r.B / Scale); }
        static long Now() => (DateTime.UtcNow.Ticks - DateTime.UnixEpoch.Ticks) / 10;
        nint Xy(double x, double y) => (nint)(((int)(y * Scale) << 16) | ((int)(x * Scale) & 0xFFFF));
        public void Post(uint msg, nint wp, nint lp, string ev) { Posts.Add(new(Now(), ev)); PostMessage(H, msg, wp, lp); }
        public void Move(double x, double y) => Post(WM_MOUSEMOVE, 0, Xy(x, y), "WM_MOUSEMOVE");
        public void Down(double x, double y) => Post(WM_LBUTTONDOWN, 1, Xy(x, y), "WM_LBUTTONDOWN");
        public void Up(double x, double y) => Post(WM_LBUTTONUP, 0, Xy(x, y), "WM_LBUTTONUP");
        public void Click(double x, double y) { Down(x, y); Up(x, y); }
        public void Key(int vk) => Post(WM_KEYDOWN, vk, 1, "WM_KEYDOWN");
        public void Char(char c) => Post(WM_CHAR, c, 1, "WM_CHAR");
        public void Wheel(int notches) => Post(WM_MOUSEWHEEL, (nint)((notches * 120) << 16), Xy(600, 400), "WM_MOUSEWHEEL");
        // Runs `step` every `ms` (spin-timed) until the scenario's time is up.
        public void Every(double ms, Action<int> step)
        {
            var sw = Stopwatch.StartNew();
            for (int i = 0; sw.ElapsedMilliseconds < Seconds * 1000; i++)
            {
                step(i);
                var next = (i + 1) * ms;
                while (sw.Elapsed.TotalMilliseconds < next) if (next - sw.Elapsed.TotalMilliseconds > 2) Thread.Sleep(1); else Thread.SpinWait(50);
            }
        }
        public double[] HeaderCenters()
        {
            var (w, _) = Size();
            var xs = new double[7];
            double x = w - PAD - BAR;
            for (int i = 5; i >= 0; i--) { xs[i + 1] = x - Widths[i] / 2; x -= Widths[i]; }
            xs[0] = (NAV + PAD + x) / 2;
            return xs;
        }
    }

    // 40 header clicks per second, every column in turn.
    static void Sort(Ctx c) { var xs = c.HeaderCenters(); c.Every(25, i => c.Click(xs[i % 7], TOP + HEAD / 2)); }
    // 250 wheel notches per second, the direction flips every 50.
    static void Wheel(Ctx c) => c.Every(4, i => c.Wheel((i / 50) % 2 == 0 ? -3 : 3));
    // The scroll thumb dragged end to end every 400 ms, a move every 2 ms.
    static void Thumb(Ctx c)
    {
        var (w, h) = c.Size();
        c.Down(w - BAR / 2, TOP + HEAD + 4);
        c.Every(2, i => { double t = (i % 200) / 100.0; c.Move(w - BAR / 2, TOP + HEAD + (t < 1 ? t : 2 - t) * (h - TOP - HEAD)); });
        c.Up(w - BAR / 2, TOP + HEAD + 4);
    }
    // A 1000 Hz mouse sweeping over the rows and the headers.
    static void Hover(Ctx c)
    {
        var (w, h) = c.Size();
        c.Every(1, i => c.Move(NAV + 40 + (i * 7) % (w - NAV - 60), TOP + ((i * 3) % (h - TOP))));
    }
    // Typing a filter and deleting it, a key every 10 ms.
    static void Type(Ctx c)
    {
        const string word = "svchost";
        c.Every(10, i => { int k = i % (word.Length * 2); if (k < word.Length) c.Char(word[k]); else c.Key(0x08); });
    }
    // Arrows, page keys, Home/End and expand/collapse, a key every 5 ms.
    static void Keys(Ctx c)
    {
        int[] keys = { 0x28, 0x28, 0x28, 0x27, 0x28, 0x27, 0x22, 0x25, 0x26, 0x21, 0x23, 0x24, 0x0D };
        c.Every(5, i => c.Key(keys[c.Rng.Next(keys.Length)]));
    }
    // Processes / Performance in the navigation pane, 30 switches per second.
    static void Tabs(Ctx c) => c.Every(33, i => c.Click(NAV / 2, TITLE + 4 + (i % 2 == 0 ? 2 : 1) * 40 + 18));
    // A column separator dragged ±150 DIPs, a move every 2 ms.
    static void Columns(Ctx c)
    {
        var (w, _) = c.Size();
        double x0 = w - PAD - BAR - Widths.Sum() + Widths[0]; // CPU|Memory
        c.Down(x0, TOP + HEAD / 2);
        c.Every(2, i => c.Move(x0 + 150 * Math.Sin(i / 40.0), TOP + HEAD / 2));
        c.Up(x0, TOP + HEAD / 2);
    }
    // The window resized every 10 ms between 700x450 and 1700x1050 pixels (a frantic edge drag).
    static void Resize(Ctx c)
    {
        GetWindowRect(c.H, out var r);
        double slowest = 0; // SetWindowPos waits for the app's WM_SIZE: a slow one stalls the drag
        c.Every(10, i =>
        {
            double t = Math.Abs(Math.Sin(i / 30.0));
            c.Posts.Add(new((DateTime.UtcNow.Ticks - DateTime.UnixEpoch.Ticks) / 10, "WM_SIZE"));
            var sw = Stopwatch.StartNew();
            Park(c.H, (int)(700 + 1000 * t), (int)(450 + 600 * t));
            slowest = Math.Max(slowest, sw.Elapsed.TotalMilliseconds);
        });
        Park(c.H, r.R - r.L, r.B - r.T);
        Console.WriteLine($"    SetWindowPos max {slowest:F1} ms");
    }
    // All of it at once, in random order, a few events per millisecond.
    static void Everything(Ctx c)
    {
        var xs = c.HeaderCenters();
        var (w, h) = c.Size();
        c.Every(1, i =>
        {
            switch (c.Rng.Next(20))
            {
                case 0: c.Click(xs[c.Rng.Next(7)], TOP + HEAD / 2); break;
                case 1: case 2: case 3: c.Wheel(c.Rng.Next(2) == 0 ? -3 : 3); break;
                case 4: c.Char("svchost"[c.Rng.Next(7)]); break;
                case 5: c.Key(0x08); break;
                case 6: case 7: c.Key(new[] { 0x26, 0x28, 0x27, 0x25, 0x21, 0x22 }[c.Rng.Next(6)]); break;
                case 8: c.Click(NAV + 60 + c.Rng.NextDouble() * 300, TOP + HEAD + c.Rng.NextDouble() * (h - TOP - HEAD)); break;
                case 9: if (c.Rng.Next(10) == 0) c.Click(NAV / 2, TITLE + 4 + (c.Rng.Next(2) + 1) * 40 + 18); break;
                default: c.Move(NAV + c.Rng.NextDouble() * (w - NAV), TITLE + c.Rng.NextDouble() * (h - TITLE)); break;
            }
        });
    }

    // Top-left of a window whose bottom-right 2x2 px sit at the top-left corner of the screen (not
    // the bottom: an auto-hidden taskbar keeps a strip there, which would cover it).
    static (int X, int Y) Parked(int width, int height)
    {
        var s = System.Windows.Forms.Screen.PrimaryScreen!.Bounds;
        return (s.Left - width + 2, s.Top - height + 2);
    }

    static void Park(nint h, int width, int height)
    {
        var (x, y) = Parked(width, height);
        SetWindowPos(h, -1, x, y, width, height, 0x0010); // HWND_TOPMOST, NOACTIVATE
    }

    record Ev(long At, string Name, long Us, long N);

    static void Report(string name, Ctx c, string tracePath)
    {
        if (!File.Exists(tracePath)) { Console.WriteLine($"{name,-11} no trace (app did not exit cleanly?)"); return; }
        var lines = File.ReadAllLines(tracePath);
        long epoch = long.Parse(lines[0].Split(' ')[1]);
        var ev = lines.Skip(1).Select(l => l.Split(' ')).Select(p => new Ev(epoch + long.Parse(p[0]), p[1], long.Parse(p[2]), long.Parse(p[3]))).ToList();
        long t0 = c.Posts.Count > 0 ? c.Posts[0].Us : 0, t1 = c.Posts.Count > 0 ? c.Posts[^1].Us : 0;
        var during = ev.Where(e => e.At >= t0 && e.At <= t1 + 200_000).ToList();
        var handlers = during.Where(e => e.Name.StartsWith("WM_")).ToList();
        var presents = ev.Where(e => e.Name == "present").Select(e => e.At + e.Us).Order().ToList();
        var draws = during.Where(e => e.Name == "draw").Select(e => e.Us / 1000.0).ToList();
        var gaps = presents.Zip(presents.Skip(1), (a, b) => (b - a) / 1000.0).Where((g, i) => presents[i] >= t0 && presents[i] <= t1).ToList();

        // Match each posted input to its handler, in order per message type.
        var lat = new List<double>();
        var behind = new List<double>(); // queue wait: post to handler start
        foreach (var g in c.Posts.GroupBy(p => p.Event))
        {
            if (g.Key == "WM_SIZE") continue; // sent, not posted: measured by the handler time
            var hs = ev.Where(e => e.Name == g.Key && e.At >= t0 - 1000).ToList();
            foreach (var (p, hd) in g.Zip(hs))
            {
                behind.Add((hd.At - p.Us) / 1000.0);
                // first present that ends after the handler started
                var end = presents.FirstOrDefault(x => x >= hd.At, -1);
                if (end > 0 && g.Key != "WM_MOUSEMOVE") lat.Add((end - p.Us) / 1000.0);
            }
        }
        double P(List<double> v, double q) { var s = v.Order().ToList(); return s.Count == 0 ? double.NaN : s[(int)Math.Min(s.Count - 1, Math.Round(q / 100 * (s.Count - 1)))]; }
        var hms = handlers.Select(e => e.Us / 1000.0).ToList();
        Console.WriteLine($"{name,-11}{c.Posts.Count,7}{$"{P(hms, 99):F1}/{(hms.Count > 0 ? hms.Max() : 0):F1}",17}{$"{P(draws, 50):F1}/{P(draws, 99):F1}",14}{(gaps.Count > 0 ? gaps.Max() : double.NaN),15:F1}{$"{P(lat, 50):F1}/{P(lat, 99):F1}/{(lat.Count > 0 ? lat.Max() : 0):F1}",24}{P(behind, 99),8:F1}");
        foreach (var g in during.GroupBy(e => e.Name).OrderByDescending(g => g.Sum(e => e.Us)))
        {
            var v = g.Select(e => e.Us / 1000.0).ToList();
            Console.WriteLine($"    {g.Key,-15} n={g.Count(),-6} total {g.Sum(e => e.Us) / 1000.0,8:F1} ms  p50 {P(v, 50),6:F2}  p99 {P(v, 99),6:F2}  max {v.Max(),7:F2}");
        }
    }

    // Short-lived console processes, replaced as they exit: the process list churns all the time.
    sealed class Churn : IDisposable
    {
        volatile bool _stop;
        readonly Thread _t;
        public Churn(int n)
        {
            _t = new Thread(() =>
            {
                var live = new List<Process>();
                var rng = new Random(2);
                while (!_stop)
                {
                    live.RemoveAll(p => { if (!p.HasExited) return false; p.Dispose(); return true; });
                    while (live.Count < n && !_stop)
                        live.Add(Process.Start(new ProcessStartInfo("ping.exe", $"-n {rng.Next(2, 12)} 127.0.0.1") { UseShellExecute = false, CreateNoWindow = true, RedirectStandardOutput = false })!);
                    Thread.Sleep(50);
                }
                foreach (var p in live) { try { p.Kill(); } catch { } p.Dispose(); }
            }) { IsBackground = true };
            _t.Start();
        }
        public void Dispose() { _stop = true; _t.Join(); }
    }

    [StructLayout(LayoutKind.Sequential)] struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern nint FindWindowEx(nint parent, nint after, string? c, string? t);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(nint h, out int pid);
    [DllImport("user32.dll")] static extern bool PostMessage(nint h, uint m, nint w, nint l);
    [DllImport("user32.dll")] static extern bool GetClientRect(nint h, out RECT r);
    [DllImport("user32.dll")] static extern bool GetWindowRect(nint h, out RECT r);
    [DllImport("user32.dll")] static extern bool SetWindowPos(nint h, nint after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] static extern uint GetDpiForWindow(nint h);
    [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(nint ctx);
    [DllImport("winmm.dll")] static extern uint timeBeginPeriod(uint ms);
}
