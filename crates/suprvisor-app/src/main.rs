//! Suprvisor: a task manager. Ctrl+Shift+Esc shows it (keyboard hook, hotkey.rs, while it runs;
//! the IFEO redirect, ifeo.rs, while it does not), and so does running suprvisor.exe again.
//! Closing the window ends the process.
//!   suprvisor.exe                 show (start it if none runs)
//!   suprvisor.exe --resident      start hidden (the Start task: the IFEO stub then shows it)
//!   suprvisor.exe --inactive      start shown without taking the focus (audit/stress.ps1)
//!   suprvisor.exe --hide          hide
//!   suprvisor.exe --exit          quit the running process
//!   suprvisor.exe --ifeo ...      run by Windows in place of taskmgr.exe (ifeo.rs)
//!   suprvisor.exe --real-taskmgr  start the real Task Manager (the fallback task)
#![windows_subsystem = "windows"]

mod edit;
mod hotkey;
mod ifeo;
mod render;
mod settings;
mod shell;
mod table;
mod theme;
mod trace;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use suprvisor_core::{Collector, Proc, System};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, ClientToScreen, EndPaint, PAINTSTRUCT, ScreenToClient};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, Result, w};

use edit::Edit;
use render::{BAR, Focus, HEAD, Hot, ROW, SEARCH_Y, TITLE, TOP};
use table::{COLS, Col, Key, NUMS, Table};

/// The window class, also the single-instance key. SUPRVISOR_TEST (audit/stress.ps1) gives a test
/// instance its own, so it runs next to the user's instance and leaves it alone.
fn class() -> PCWSTR {
    if test() { w!("SuprvisorTest") } else { w!("Suprvisor") }
}

pub fn test() -> bool {
    std::env::var_os("SUPRVISOR_TEST").is_some()
}
const WM_APP_SHOW: u32 = WM_APP + 1;
const WM_APP_HIDE: u32 = WM_APP + 2;
const WM_APP_EXIT: u32 = WM_APP + 3;
const WM_APP_DATA: u32 = WM_APP + 4; // the collector thread has a new snapshot
const WM_APP_ICONS: u32 = WM_APP + 5; // the icon thread loaded more icons
const WM_APP_HOTKEY: u32 = WM_APP + 6; // Ctrl+Shift+Esc, from the keyboard hook
// Test instances only (SUPRVISOR_TEST): the harness posts input as messages, so it never moves the
// user's mouse or takes the keyboard focus. These carry what posted messages cannot.
const WM_APP_TEST_MODS: u32 = WM_APP + 7; // wParam: 1 Ctrl, 2 Shift, 4 Alt, held from now on
const WM_APP_TEST_SNAP: u32 = WM_APP + 8; // write the next frame to %TEMP%\suprvisor-test.bmp
static TEST_MODS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const TIMER_SAVE: usize = 1;
const TIMER_CARET: usize = 2;
const TIMER_SHOW: usize = 3;
const MENU_END: usize = 10;
const MENU_OPEN: usize = 11;
const MENU_COPY_PATH: usize = 12;
const MENU_COPY_CMD: usize = 13;
const MENU_GROUPED: usize = 14;
const MENU_RESET: usize = 15;
const MENU_COLUMN: usize = 20; // + Col::num
const MENU_UNDO: usize = 30;
const MENU_CUT: usize = 31;
const MENU_COPY: usize = 32;
const MENU_PASTE: usize = 33;
const MENU_DELETE: usize = 34;
const MENU_SELECT_ALL: usize = 35;
/// The caret stops blinking after this long without input, like Windows' own text boxes can; each
/// blink is a repaint.
const CARET_BLINK_FOR: Duration = Duration::from_secs(10);

/// What the collector thread hands to the UI thread.
#[derive(Clone, Default)]
struct Snapshot {
    system: System,
    procs: Arc<Vec<Proc>>,
    titles: Arc<HashMap<u32, String>>,
    icons: Arc<HashMap<String, Arc<shell::Icon>>>,
    history: Arc<VecDeque<System>>, // one sample per second, oldest first
}

enum Signal {
    Visible(bool),
    Rescan,      // a process was ended: list again now
    Table(bool), // the process table is on screen (else the Performance tab)
}

enum Drag {
    None,
    /// Pressed on a header at this x: a release sorts, a move drags the column to a new place.
    Header(Col, f32),
    /// A column follows the mouse; the value is the mouse x minus the column's left edge.
    Reorder(Col, f32),
    /// A separator: the columns on its two sides trade width. Mouse x, widths and Name width at
    /// the start.
    Column { left: Option<usize>, right: usize, x: f32, start: [f32; 6], name: f32 },
    Thumb(f32), // mouse y minus the thumb top
    /// Pressed on a row at this y: a move selects the rows it passes.
    Row(usize, f32),
    Select { anchor: usize, base: HashSet<Key> },
    Text, // selecting text in the search box
}

struct App {
    hwnd: HWND,
    renderer: render::Renderer,
    theme: theme::Theme,
    data: Arc<Mutex<Snapshot>>,
    wanted: Arc<Mutex<Vec<(u32, i64)>>>, // processes under the visible rows: refreshed fast
    signal: mpsc::Sender<Signal>,
    snap: Snapshot,
    got_data: bool,
    table: Table,
    settings: settings::Settings,
    performance: bool, // the Performance tab is open
    nav_open: bool,    // the navigation pane shows labels
    edit: Edit,
    focus: Option<Focus>,
    focus_visible: bool, // the keyboard moved the focus last: draw the focus ring
    active: bool,        // the window has the keyboard
    caret_on: bool,
    caret_at: Instant, // last input in the search box: the caret blinks for CARET_BLINK_FOR after it
    caret_timer: bool,
    hot: Hot,
    mouse: Option<(f32, f32)>,
    caption_down: u32,         // caption button pressed (HTCLOSE...), 0 for none
    save_due: Option<Instant>, // settings changed: write them at this time (TIMER_SAVE checks)
    dirty: bool,               // a frame is due (the message loop draws it)
    modal: u32,                // inside a size/move, menu or dialog loop, which does not return to ours: paint at once
    drag: Drag,
    // Animation: values that move toward a target a little on each frame.
    scroll_to: f32,
    nav_w: f32,
    nav_pill: f32,
    col_off: [f32; 7], // per column (COLS order): drawn left edge minus its place
    last_frame: Instant,
    app_icon: Option<shell::Icon>,
    pending_show: Option<bool>, // show asked before the first data: shows when it comes (or soon)
    maximize_on_show: bool,     // the window was maximized when it last closed
    shown: bool,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = args.first().map(String::as_str);
    match arg {
        Some("--ifeo") => return ifeo::redirect(&args[1..]),
        Some("--real-taskmgr") => return ifeo::real_taskmgr(&args[1..]),
        _ => {}
    }
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)? };
    let resident = arg == Some("--resident");
    let msg = match arg {
        Some("--hide") => WM_APP_HIDE,
        Some("--exit") => WM_APP_EXIT,
        _ => WM_APP_SHOW,
    };
    // One is running: signal it and leave. This process got the user's input, so it may pass the
    // right to take the foreground to the running one.
    if let Ok(existing) = unsafe { FindWindowW(class(), None) } {
        if resident {
            return Ok(());
        }
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(existing, Some(&mut pid)) };
        let _ = unsafe { AllowSetForegroundWindow(pid) };
        unsafe { PostMessageW(Some(existing), msg, WPARAM(0), LPARAM(0))? };
        return Ok(());
    }
    if msg != WM_APP_SHOW {
        return Ok(()); // --hide / --exit with nothing running
    }
    // Installed: run elevated through the Start task (no UAC prompt), like the logon start was.
    if !resident && !test() && !ifeo::elevated() && ifeo::start_elevated() {
        return Ok(());
    }

    // Like Task Manager: the window must answer when the machine is saturated.
    let _ = unsafe { SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS) };
    let instance = unsafe { GetModuleHandleW(None)? };
    let icon = |size: i32| unsafe { LoadImageW(Some(instance.into()), PCWSTR(1 as _), IMAGE_ICON, size, size, LR_DEFAULTCOLOR) }.ok().map(|h| HICON(h.0));
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        hIcon: icon(unsafe { GetSystemMetrics(SM_CXICON) }).unwrap_or_default(),
        hIconSm: icon(unsafe { GetSystemMetrics(SM_CXSMICON) }).unwrap_or_default(),
        lpszClassName: class(),
        ..Default::default()
    };
    unsafe { RegisterClassExW(&wc) };

    let (signal_tx, signal_rx) = mpsc::channel();
    let data = Arc::new(Mutex::new(Snapshot::default()));
    let wanted = Arc::new(Mutex::new(Vec::new()));
    let settings = settings::Settings::load();
    let (window, performance) = (settings.window, settings.performance);
    let nav_w = render::nav_width(settings.nav);
    let app = Box::new(App {
        hwnd: HWND::default(),
        renderer: render::Renderer::new()?,
        theme: theme::Theme::current(),
        data: data.clone(),
        wanted: wanted.clone(),
        signal: signal_tx,
        snap: Snapshot::default(),
        got_data: false,
        table: Table::new(settings.sort, settings.desc, settings.grouped),
        performance: settings.performance,
        nav_open: settings.nav,
        edit: Edit::default(),
        focus: None,
        focus_visible: false,
        active: false,
        caret_on: true,
        caret_at: Instant::now(),
        caret_timer: false,
        hot: Hot::None,
        mouse: None,
        caption_down: 0,
        save_due: None,
        dirty: false,
        modal: 0,
        settings,
        drag: Drag::None,
        scroll_to: 0.0,
        nav_w,
        nav_pill: 0.0,
        col_off: [0.0; 7],
        last_frame: Instant::now(),
        app_icon: shell::app_icon(32),
        pending_show: None,
        maximize_on_show: false,
        shown: false,
    });
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP, // all content comes through DirectComposition (render.rs)
            class(),
            w!("Suprvisor"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1200,
            800,
            None,
            None,
            Some(instance.into()),
            Some(Box::into_raw(app).cast()),
        )?
    };
    if let Some(([left, top, right, bottom], _)) = window {
        // Windows moves the rect onto a monitor if it is off screen.
        let p = WINDOWPLACEMENT {
            length: size_of::<WINDOWPLACEMENT>() as u32,
            showCmd: SW_HIDE.0 as u32,
            rcNormalPosition: RECT { left, top, right, bottom },
            ..Default::default()
        };
        let _ = unsafe { SetWindowPlacement(hwnd, &p) };
    }
    style_window(hwnd, theme::Theme::current().dark);
    // WM_NCCALCSIZE at creation does not size the client area: ask again, so the title bar goes.
    let _ = unsafe { SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE) };
    spawn_collector(hwnd, data, wanted, signal_rx, !performance, !resident);
    hotkey::install(hwnd, WM_APP_HOTKEY);
    // Started elevated by the Start task; the IFEO stub and suprvisor.exe started from Explorer
    // are not. UIPI drops their messages unless allowed here.
    for m in [WM_APP_SHOW, WM_APP_HIDE, WM_APP_EXIT] {
        let _ = unsafe { ChangeWindowMessageFilterEx(hwnd, m, MSGFLT_ALLOW, None) };
    }
    if let Some(a) = self::app(hwnd) {
        a.nav_pill = a.pill_target();
        if window.is_some_and(|(_, max)| max) {
            a.maximize_on_show = true;
        }
        if !resident {
            a.show_with(arg != Some("--inactive"));
        }
    }

    // Message loop. Input never waits for the display: all pending messages run first, then at most
    // one frame, drawn when the swapchain can take it (its waitable object, render.rs). Many
    // changes between two frames cost one paint.
    let mut m = MSG::default();
    loop {
        while unsafe { PeekMessageW(&mut m, None, 0, 0, PM_REMOVE) }.as_bool() {
            if m.message == WM_QUIT {
                return Ok(());
            }
            unsafe {
                // A test harness key (bit 25 of lParam, reserved) is not translated: its WM_CHAR
                // would come from the real keyboard state, not the posted modifiers.
                if !(m.message == WM_KEYDOWN && m.lParam.0 & 1 << 25 != 0 && test()) {
                    let _ = TranslateMessage(&m);
                }
                DispatchMessageW(&m);
            }
        }
        let Some(a) = self::app(hwnd) else { return Ok(()) };
        a.dirty &= a.shown; // hidden: nothing to draw (the swapchain is made at the first show)
        let wait = if a.dirty { a.renderer.waitable() } else { None };
        if a.dirty && wait.is_none() {
            a.paint(); // no swapchain yet: this paint makes it
            continue;
        }
        let handles = wait.as_slice();
        let r = unsafe { MsgWaitForMultipleObjectsEx(Some(handles), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
        if !handles.is_empty() && r == WAIT_OBJECT_0 {
            a.renderer.ready = true; // the wait took the frame slot
            a.paint();
        }
    }
}

/// Collector thread. Hidden, or on the Performance tab: machine totals once per second (the graph
/// history moves once per second, and each repaint costs ~4 ms of CPU). With the process table on
/// screen: totals every 500 ms, and every 500 ms the processes
/// under the visible rows; every 3 s the process list, window titles, new icons; every 60 s the
/// kernel list (per-process disk I/O). See suprvisor-core.
/// Once at start, hidden or not, the full list: the first show then has rows at once. Shown at
/// start (`fast`), that first list is the quick handle scan (~20 ms), so the window shows full
/// sooner. Icons load on their own thread and come in as they load, apps first.
fn spawn_collector(hwnd: HWND, data: Arc<Mutex<Snapshot>>, wanted: Arc<Mutex<Vec<(u32, i64)>>>, signal: mpsc::Receiver<Signal>, table: bool, fast: bool) {
    const SCAN: Duration = Duration::from_secs(3);
    const FULL: Duration = Duration::from_secs(60);
    const HISTORY: usize = 60; // seconds on the Performance graphs
    let hwnd = hwnd.0 as usize; // HWND is not Send; the value is valid on any thread
    let icons = spawn_icons(hwnd, data.clone());
    std::thread::spawn(move || {
        // The process runs at high priority so the window stays smooth under full load; the
        // collector can wait. LOWEST in the high class is still above normal-priority processes.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST) };
        let Ok(mut c) = Collector::new() else { return };
        let mut procs: Vec<Proc> = Vec::new();
        let mut index: HashMap<(u32, i64), usize> = HashMap::new();
        let mut snap = Snapshot::default();
        let mut tried: HashSet<String> = HashSet::new();
        let mut history: VecDeque<System> = VecDeque::with_capacity(HISTORY);
        let (mut shown, mut table, mut tick) = (false, table, 0u64);
        let (mut primed, mut quick) = (false, fast); // quick: the next list skips the kernel list
        let (mut scanned, mut full, mut engines, mut sampled): (Option<Instant>, Option<Instant>, Option<Instant>, Option<Instant>) = (None, None, None, None);
        let mut next = Instant::now();
        loop {
            match signal.recv_timeout(next.saturating_duration_since(Instant::now())) {
                Ok(Signal::Visible(v)) => {
                    // The list is stale after a hidden period: the next scan (now, if the last one is
                    // older than SCAN) skips the slow kernel list. Not a forced scan: a show/hide
                    // spammer would otherwise keep this thread scanning.
                    let now = v && !shown;
                    (quick, shown) = (quick || now, v);
                    if !now {
                        continue; // a resize step or a hide: the next tick is soon enough
                    }
                }
                Ok(Signal::Table(t)) => {
                    let now = t && !table; // not listed while the Performance tab was open
                    (quick, table) = (quick || now, t);
                    if !now {
                        continue;
                    }
                }
                Ok(Signal::Rescan) => scanned = None,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            let listing = (shown && table) || !primed;
            tick += 1;
            if !listing {
            } else if scanned.is_none_or(|t| t.elapsed() >= SCAN) {
                let quick = std::mem::take(&mut quick);
                let t = Instant::now();
                procs = if !quick && full.is_none_or(|t| t.elapsed() >= FULL) {
                    full = Some(Instant::now());
                    let p = c.scan_full().unwrap_or_else(|_| c.scan());
                    trace::record("scan_full", t, p.len() as u64);
                    p
                } else {
                    let p = c.scan();
                    trace::record("scan", t, p.len() as u64);
                    p
                };
                scanned = Some(Instant::now());
                let t = Instant::now();
                index = procs.iter().enumerate().map(|(i, p)| ((p.pid, p.create_time), i)).collect();
                snap.titles = Arc::new(shell::window_titles());
                // New exe paths go to the icon thread, apps (a window title) first.
                let mut new: Vec<(bool, String)> = procs
                    .iter()
                    .filter(|p| !p.path.is_empty() && tried.insert(p.path.clone()))
                    .map(|p| (!snap.titles.contains_key(&p.pid), p.path.clone()))
                    .collect();
                new.sort();
                for (_, path) in new {
                    let _ = icons.send(path);
                }
                trace::record("titles", t, snap.titles.len() as u64);
            } else {
                // Disk I/O is slow to read (~150 k cycles per process): every 3rd tick.
                let io = tick % 3 == 0;
                let keys = wanted.lock().unwrap().clone(); // do not hold the lock: the UI thread writes it
                let t = Instant::now();
                for k in &keys {
                    if let Some(&i) = index.get(k) {
                        c.refresh(std::slice::from_mut(&mut procs[i]), io);
                    }
                }
                trace::record("refresh", t, keys.len() as u64);
            }
            // Idle GPU engines cost ~10 ms to read: every 3 s while visible, every 60 s while hidden.
            let all = engines.is_none_or(|t| t.elapsed() >= if shown { SCAN } else { FULL });
            if all {
                engines = Some(Instant::now());
            }
            let t = Instant::now();
            snap.system = c.system(all);
            trace::record("system", t, all as u64);
            if sampled.is_none_or(|t| t.elapsed() >= Duration::from_millis(900)) {
                sampled = Some(Instant::now());
                if history.len() == HISTORY {
                    history.pop_front();
                }
                history.push_back(snap.system.clone());
                snap.history = Arc::new(history.clone());
            }
            if listing {
                snap.procs = Arc::new(procs.clone());
            }
            {
                let mut d = data.lock().unwrap();
                snap.icons = d.icons.clone(); // the icon thread's
                *d = snap.clone();
            }
            if shown || !std::mem::replace(&mut primed, true) {
                let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_DATA, WPARAM(0), LPARAM(0)) };
            }
            next = Instant::now() + Duration::from_millis(if shown && table { 500 } else { 1000 });
        }
    });
}

/// Icon thread: loads the icon of each exe path it gets (~1-5 ms each; ~1-2 s for all at start)
/// and publishes them in batches, so the rows fill in while the rest load.
fn spawn_icons(hwnd: usize, data: Arc<Mutex<Snapshot>>) -> mpsc::Sender<String> {
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST) };
        let mut icons: HashMap<String, Arc<shell::Icon>> = HashMap::new();
        while let Ok(first) = rx.recv() {
            let t = Instant::now();
            let mut path = Some(first);
            while let Some(p) = path {
                if let Some(i) = shell::icon(&p) {
                    icons.insert(p, Arc::new(i));
                }
                path = if t.elapsed() < Duration::from_millis(50) { rx.try_recv().ok() } else { None };
            }
            data.lock().unwrap().icons = Arc::new(icons.clone());
            trace::record("icons", t, icons.len() as u64);
            let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_ICONS, WPARAM(0), LPARAM(0)) };
        }
    });
    tx
}

fn style_window(hwnd: HWND, dark: bool) {
    let set = |attr: DWMWINDOWATTRIBUTE, v: i32| unsafe {
        let _ = DwmSetWindowAttribute(hwnd, attr, (&raw const v).cast(), 4);
    };
    set(DWMWA_USE_IMMERSIVE_DARK_MODE, dark as i32);
    set(DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW.0);
    // No open/close animation: the window must be on screen in the first frame after the hotkey.
    set(DWMWA_TRANSITIONS_FORCEDISABLED, 1);
    let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
    let _ = unsafe { DwmExtendFrameIntoClientArea(hwnd, &m) };
}

fn app<'a>(hwnd: HWND) -> Option<&'a mut App> {
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut App;
    unsafe { p.as_mut() }
}

fn dpi(hwnd: HWND) -> u32 {
    unsafe { GetDpiForWindow(hwnd) }
}

/// Resize border height in pixels: the part of the title bar that resizes the window.
fn frame(hwnd: HWND) -> i32 {
    let d = dpi(hwnd);
    unsafe { GetSystemMetricsForDpi(SM_CYFRAME, d) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, d) }
}

/// The caption button of a non-client hit code.
fn caption(wp: WPARAM) -> Option<usize> {
    [HTMINBUTTON, HTMAXBUTTON, HTCLOSE].iter().position(|&h| h == wp.0 as u32)
}

fn down(vk: VIRTUAL_KEY) -> bool {
    if test() {
        let m = TEST_MODS.load(std::sync::atomic::Ordering::Relaxed);
        let bit = match vk {
            VK_CONTROL => 1,
            VK_SHIFT => 2,
            VK_MENU => 4,
            _ => 0,
        };
        if m & bit != 0 {
            return true;
        }
    }
    (unsafe { GetKeyState(vk.0 as i32) }) < 0
}

fn ctrl() -> bool {
    down(VK_CONTROL)
}

fn shift() -> bool {
    down(VK_SHIFT)
}

/// Index of a column in `COLS`.
fn ci(c: Col) -> usize {
    COLS.iter().position(|&k| k == c).unwrap_or(0)
}

/// Moves `v` toward `to` with a time constant of 1/`rate` seconds. True while it still moves.
fn approach(v: &mut f32, to: f32, rate: f32, dt: f32, settle: f32) -> bool {
    *v += (to - *v) * (1.0 - (-dt * rate).exp());
    if (to - *v).abs() < settle {
        *v = to;
        return false;
    }
    true
}

enum Hit {
    None,
    Header(Col),
    Edge(usize),      // separator at the left of shown column k (k >= 1): drag to resize
    Row(usize, bool), // row, on its chevron
    Thumb,
    Track,
    Nav(usize), // 0 the menu button, 1 Processes, 2 Performance, 3 Windows Task Manager
    Search,
    SearchClear,
    Command(usize), // 0 Run new task, 1 End task
    Caption(usize), // 0 minimize, 1 maximize, 2 close
    Title,          // empty title bar: drags the window
}

impl App {
    /// Client size in DIPs.
    fn size(&self) -> (f32, f32) {
        let mut rc = RECT::default();
        let _ = unsafe { GetClientRect(self.hwnd, &mut rc) };
        let s = 96.0 / dpi(self.hwnd) as f32;
        (rc.right as f32 * s, rc.bottom as f32 * s)
    }

    /// Mouse position from a mouse message, in DIPs.
    fn point(&self, lp: LPARAM) -> (f32, f32) {
        let s = 96.0 / dpi(self.hwnd) as f32;
        ((lp.0 & 0xFFFF) as i16 as f32 * s, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 * s)
    }

    fn page(&self) -> usize {
        render::rows_visible(self.size().1).max(1)
    }

    /// Shown columns at their places.
    fn layout_cols(&self) -> Vec<(Col, f32, f32)> {
        let s = &self.settings;
        render::columns(self.nav_w, self.size().0, &s.widths, &s.order, &s.hidden)
    }

    /// Shown columns where they are drawn: sliding to their places, or following the mouse.
    fn drawn_cols(&self) -> Vec<(Col, f32, f32)> {
        let mut cols = self.layout_cols();
        for c in &mut cols {
            let x = match self.drag {
                Drag::Reorder(d, grab) if d == c.0 => self.mouse.map_or(c.1, |m| m.0 - grab),
                _ => c.1 + self.col_off[ci(c.0)],
            };
            (c.1, c.2) = (x, x + c.2 - c.1);
        }
        cols
    }

    /// Runs `f`, which moves or shows columns; the columns slide from where they were drawn.
    fn relayout(&mut self, f: impl FnOnce(&mut Self)) {
        let before = self.drawn_cols();
        f(self);
        for (c, x0, _) in self.layout_cols() {
            self.col_off[ci(c)] = before.iter().find(|b| b.0 == c).map_or(0.0, |b| b.1 - x0);
        }
        self.dirty = true;
    }

    fn hit(&self, x: f32, y: f32) -> Hit {
        let (w, h) = self.size();
        if y < TITLE {
            if let Some(k) = render::caption_at(x, y, w) {
                return Hit::Caption(k);
            }
            let (l, r) = render::search(w);
            return match (l..r).contains(&x) && (SEARCH_Y.0..SEARCH_Y.1).contains(&y) {
                true if !self.edit.text.is_empty() && x >= r - render::CLEAR => Hit::SearchClear,
                true => Hit::Search,
                false => Hit::Title,
            };
        }
        if let Some(k) = render::nav_at(x, y, self.nav_w, h) {
            return Hit::Nav(k);
        }
        if x < self.nav_w {
            return Hit::None;
        }
        if y < TOP {
            return render::commands(w, self.performance)
                .into_iter()
                .find(|(_, r)| (r.left..r.right).contains(&x) && (r.top..r.bottom).contains(&y))
                .map_or(Hit::None, |(k, _)| Hit::Command(k));
        }
        if self.performance {
            return Hit::None;
        }
        let rows_top = TOP + HEAD;
        if x >= w - BAR && y >= rows_top {
            return match render::thumb(h, self.table.rows.len(), self.table.scroll) {
                Some((y0, y1)) if (y0..y1).contains(&y) => Hit::Thumb,
                Some(_) => Hit::Track,
                None => Hit::None,
            };
        }
        let cols = self.layout_cols();
        if (TOP..rows_top).contains(&y) {
            if let Some(k) = (1..cols.len()).find(|&k| (x - cols[k].1).abs() <= 6.0) {
                return Hit::Edge(k);
            }
            return cols.iter().find(|c| (c.1..c.2).contains(&x) || (c.0 == Col::Name && x < c.2)).map_or(Hit::None, |c| Hit::Header(c.0));
        }
        if let Some(i) = self.row_at(y) {
            let row = &self.table.rows[i];
            let cx = render::chevron_x(row, cols[0].1);
            return Hit::Row(i, row.expandable && (cx - 2.0..cx + 18.0).contains(&x));
        }
        Hit::None
    }

    fn row_at(&self, y: f32) -> Option<usize> {
        let top = TOP + HEAD;
        if y < top {
            return None;
        }
        let i = (self.table.scroll + (y - top) / ROW).floor() as usize;
        (i < self.table.rows.len()).then_some(i)
    }

    /// After the rows or the scroll position change: tell the collector what is on screen, repaint.
    fn changed(&mut self) {
        let max = self.table.rows.len().saturating_sub(self.page()) as f32;
        self.scroll_to = self.scroll_to.clamp(0.0, max);
        self.table.scroll = self.table.scroll.clamp(0.0, max);
        let (a, b) = (self.table.scroll.min(self.scroll_to), self.table.scroll.max(self.scroll_to));
        let keys = self.table.keys(a.floor() as usize..b.ceil() as usize + self.page() + 1);
        *self.wanted.lock().unwrap() = keys;
        self.dirty = true; // a flag, not InvalidateRect: no call into the window manager per input
    }

    fn pill_target(&self) -> f32 {
        let (t, b) = render::nav_row(if self.performance { 2 } else { 1 }, self.size().1);
        (t + b) / 2.0
    }

    /// Moves the animated values one frame on. True while any still moves.
    fn animate(&mut self) -> bool {
        let now = Instant::now();
        // After an idle gap, the first frame of a motion steps as if one frame passed.
        let gap = (now - self.last_frame).as_secs_f32();
        let dt = if gap > 0.05 { 0.010 } else { gap };
        self.last_frame = now;
        let mut moving = false;
        if let Drag::Select { .. } = self.drag {
            moving |= self.autoscroll(dt);
        }
        let before = self.table.scroll;
        if !matches!(self.drag, Drag::Thumb(_)) {
            moving |= approach(&mut self.table.scroll, self.scroll_to, 22.0, dt, 0.002);
        }
        if self.table.scroll != before {
            self.changed();
            // The rows moved under a still mouse: the hover follows.
            if let (Some((_, y)), Drag::None) = (self.mouse, &self.drag) {
                self.table.hover = self.row_at(y);
            }
        }
        let nav_before = self.nav_w;
        moving |= approach(&mut self.nav_w, render::nav_width(self.nav_open), 20.0, dt, 0.3);
        if self.nav_w != nav_before {
            self.changed();
        }
        let pill = self.pill_target();
        moving |= approach(&mut self.nav_pill, pill, 20.0, dt, 0.3);
        for o in &mut self.col_off {
            moving |= approach(o, 0.0, 18.0, dt, 0.3);
        }
        moving
    }

    /// While a selection drag is above or below the rows: scrolls toward the mouse, faster the
    /// farther it is, and selects up to the row under it.
    fn autoscroll(&mut self, dt: f32) -> bool {
        let (Some((_, y)), Drag::Select { anchor, base }) = (self.mouse, &self.drag) else { return false };
        let (_, h) = self.size();
        let (top, bottom) = (TOP + HEAD, h);
        let d = if y < top { y - top } else if y > bottom { y - bottom } else { 0.0 };
        let (anchor, base) = (*anchor, base.clone());
        if d != 0.0 {
            let speed = (d.abs() / ROW * 10.0).clamp(4.0, 80.0) * d.signum(); // rows per second
            self.scroll_to += speed * dt;
            self.changed();
            self.table.scroll = self.scroll_to;
        }
        self.select_drag(anchor, y.clamp(top, bottom - 1.0), &base);
        d != 0.0
    }

    fn select_drag(&mut self, anchor: usize, y: f32, base: &HashSet<Key>) {
        let last = self.table.rows.len().saturating_sub(1);
        let top = TOP + HEAD;
        let j = ((self.table.scroll + (y - top) / ROW).floor().max(0.0) as usize).min(last);
        self.table.mark_range(anchor, j, base);
        self.table.selected = self.table.rows.get(j).map(|r| r.key.clone());
        self.dirty = true;
    }

    fn paint(&mut self) {
        self.dirty = false;
        let moving = self.animate();
        let cols = self.drawn_cols();
        let marked = self.end_rows().len();
        let frame = render::Frame {
            table: &self.table,
            system: &self.snap.system,
            icons: &self.snap.icons,
            app_icon: self.app_icon.as_ref(),
            cols: &cols,
            dragging: if let Drag::Reorder(c, _) = self.drag { Some(c) } else { None },
            processes: self.snap.procs.len(),
            performance: self.performance,
            history: &self.snap.history,
            nav_w: self.nav_w,
            nav_pill: self.nav_pill,
            search: &self.edit,
            caret: self.caret_on && self.active,
            focus: self.focus,
            focus_visible: self.focus_visible && self.active,
            hot: self.hot,
            can_end: marked > 0,
            ending: marked,
            maximized: unsafe { IsZoomed(self.hwnd) }.as_bool(),
        };
        let _ = self.renderer.paint(self.hwnd, dpi(self.hwnd), &self.theme, &frame);
        self.dirty |= moving;
    }

    /// Writes the settings file a second after the last change: a burst of sort clicks or a column
    /// drag writes it once, and no click waits on the disk (or on an antivirus scan of the file).
    /// The timer is armed once per burst: SetTimer goes into the window manager, which can make
    /// the UI thread wait when other processes hold it.
    fn save_later(&mut self) {
        if self.save_due.replace(Instant::now() + Duration::from_secs(1)).is_none() {
            unsafe { SetTimer(Some(self.hwnd), TIMER_SAVE, 250, None) };
        }
    }

    /// Takes the collector's latest snapshot.
    fn load(&mut self) {
        self.snap = self.data.lock().unwrap().clone();
        self.got_data |= !self.snap.procs.is_empty();
        self.table.procs = self.snap.procs.clone();
        self.table.titles = self.snap.titles.clone();
        if self.performance {
            self.dirty = true;
        } else {
            self.rebuild();
        }
        self.paint_in_modal();
    }

    /// Inside a modal loop (a menu, the Run dialog) our loop does not run: paint now.
    fn paint_in_modal(&mut self) {
        if self.modal > 0 && self.dirty && self.shown {
            self.paint();
        }
    }

    fn set_hot(&mut self, hot: Hot) {
        if hot != self.hot {
            self.hot = hot;
            self.dirty = true;
        }
    }

    /// Moves the keyboard focus; `keyboard`: the keyboard moved it, so the focus ring shows.
    fn set_focus(&mut self, focus: Option<Focus>, keyboard: bool) {
        if focus == Some(Focus::Search) && self.focus != focus {
            self.caret_input();
        }
        self.focus = focus;
        self.focus_visible = keyboard;
        self.dirty = true;
    }

    /// Input in the search box: the caret shows and blinks again.
    fn caret_input(&mut self) {
        self.caret_on = true;
        self.caret_at = Instant::now();
        if !self.caret_timer {
            self.caret_timer = true;
            unsafe { SetTimer(Some(self.hwnd), TIMER_CARET, GetCaretBlinkTime().clamp(200, 1200), None) };
        }
        self.dirty = true;
    }

    /// Rows End task acts on: the selected rows that are not section headers.
    fn end_rows(&self) -> Vec<usize> {
        if self.performance {
            return Vec::new();
        }
        self.table.marked_rows().into_iter().filter(|&i| !matches!(self.table.rows[i].key, Key::Section(_))).collect()
    }

    fn rebuild(&mut self) {
        let t = Instant::now();
        self.table.rebuild();
        trace::record("rebuild", t, self.table.rows.len() as u64);
        self.changed();
    }

    fn scroll_by(&mut self, n: f32) {
        self.scroll_to += n;
        self.changed();
    }

    fn ensure_visible(&mut self, i: usize) {
        let page = self.page() as f32;
        let i = i as f32;
        if i < self.scroll_to {
            self.scroll_to = i;
        } else if i + 1.0 > self.scroll_to + page {
            self.scroll_to = i + 1.0 - page;
        }
        self.changed();
    }

    fn key_at(&self, i: usize) -> Option<Key> {
        self.table.rows.get(i).map(|r| r.key.clone())
    }

    /// Selects row `i` alone.
    fn select(&mut self, i: usize) {
        let Some(k) = self.key_at(i) else { return };
        self.table.marked = HashSet::from([k.clone()]);
        (self.table.selected, self.table.anchor) = (Some(k.clone()), Some(k));
        self.ensure_visible(i);
    }

    /// Selects from the anchor row to row `i` (Shift).
    fn extend(&mut self, i: usize) {
        let Some(k) = self.key_at(i) else { return };
        let a = self.table.index_of(&self.table.anchor).unwrap_or(i);
        self.table.mark_range(a, i, &HashSet::new());
        self.table.selected = Some(k);
        self.ensure_visible(i);
    }

    /// Moves the cursor to row `i` and leaves the selection as it is (Ctrl).
    fn move_cursor(&mut self, i: usize) {
        self.table.selected = self.key_at(i);
        self.ensure_visible(i);
    }

    /// Adds row `i` to the selection or takes it out (Ctrl+click, Ctrl+Space).
    fn toggle_mark(&mut self, i: usize) {
        let Some(k) = self.key_at(i) else { return };
        if !self.table.marked.remove(&k) {
            self.table.marked.insert(k.clone());
        }
        (self.table.selected, self.table.anchor) = (Some(k.clone()), Some(k));
        self.ensure_visible(i);
    }

    fn set_tab(&mut self, performance: bool) {
        if performance == self.performance {
            return;
        }
        self.performance = performance;
        self.drag = Drag::None;
        let _ = self.signal.send(Signal::Table(!performance));
        if performance && matches!(self.focus, Some(Focus::Header(_) | Focus::Rows | Focus::Command(1))) {
            self.focus = Some(Focus::Nav(2));
        }
        if performance {
            self.dirty = true;
        } else {
            self.rebuild();
        }
        self.save_later();
    }

    fn save(&mut self) {
        let s = &mut self.settings;
        (s.sort, s.desc, s.grouped, s.performance, s.nav) = (self.table.sort, self.table.desc, self.table.grouped, self.performance, self.nav_open);
        let mut p = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        if unsafe { GetWindowPlacement(self.hwnd, &mut p) }.is_ok() {
            let r = p.rcNormalPosition;
            s.window = Some(([r.left, r.top, r.right, r.bottom], unsafe { IsZoomed(self.hwnd) }.as_bool()));
        }
        s.save();
    }

    fn show(&mut self) {
        self.show_with(true);
    }

    fn show_with(&mut self, activate: bool) {
        if !self.shown {
            self.load(); // the collector kept the totals current while hidden, without telling the UI
        }
        if !self.got_data && !self.performance {
            // Started a moment ago: the first list comes within ~50 ms. Showing then, not now,
            // avoids an empty first frame.
            if self.pending_show.replace(activate).is_none() {
                unsafe { SetTimer(Some(self.hwnd), TIMER_SHOW, 400, None) };
            }
            return;
        }
        unsafe {
            // SW_SHOW leaves a minimized window minimized, and the foreground request then only
            // flashes its taskbar button.
            let cmd = if IsIconic(self.hwnd).as_bool() {
                SW_RESTORE
            } else if std::mem::take(&mut self.maximize_on_show) {
                SW_SHOWMAXIMIZED
            } else if activate {
                SW_SHOW
            } else {
                SW_SHOWNOACTIVATE
            };
            let _ = ShowWindow(self.hwnd, cmd);
            if activate && (!SetForegroundWindow(self.hwnd).as_bool() || GetForegroundWindow() != self.hwnd) {
                // Foreground lock: after the hook swallowed the key, this process did not get the
                // last input. Joining the foreground thread's input queue lifts the lock.
                let fg = GetWindowThreadProcessId(GetForegroundWindow(), None);
                let me = GetCurrentThreadId();
                let _ = AttachThreadInput(me, fg, true);
                let _ = SetForegroundWindow(self.hwnd);
                let _ = AttachThreadInput(me, fg, false);
            }
        }
        if !self.shown {
            self.shown = true;
            let _ = self.signal.send(Signal::Visible(true));
        }
    }

    fn hide(&mut self) {
        self.save();
        let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        self.shown = false;
        let _ = self.signal.send(Signal::Visible(false));
    }

    fn close(&mut self) {
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }

    fn cursor_proc(&self) -> Option<&Proc> {
        let row = &self.table.rows[self.table.selected_index()?];
        row.rep.map(|i| &self.table.procs[i])
    }

    /// Ends every selected process, and every process of each selected group.
    fn end_task(&mut self) {
        let rows = self.end_rows();
        if rows.is_empty() {
            return;
        }
        let mut targets: Vec<(u32, i64)> = Vec::new();
        for &i in &rows {
            match self.table.rows[i].key {
                Key::Proc(pid, created) => targets.push((pid, created)),
                Key::Group(..) => targets.extend(self.table.keys(i..i + 1)),
                Key::Section(_) => {}
            }
        }
        targets.sort_unstable();
        targets.dedup();
        let critical: Vec<String> = targets
            .iter()
            .filter(|&&(pid, _)| suprvisor_core::is_critical(pid))
            .filter_map(|&(pid, _)| self.table.procs.iter().find(|p| p.pid == pid).map(|p| format!("{} ({pid})", p.name)))
            .collect();
        if !critical.is_empty() {
            let text = HSTRING::from(format!(
                "{} {} a critical Windows process. Ending it stops Windows immediately and unsaved data is lost.\n\nEnd it anyway?",
                critical.join(", "),
                if critical.len() == 1 { "is" } else { "are each" }
            ));
            self.modal += 1;
            let answer = unsafe { MessageBoxW(Some(self.hwnd), &text, w!("Suprvisor"), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) };
            self.modal -= 1;
            if answer != IDYES {
                return;
            }
        }
        if targets.iter().map(|&(pid, created)| suprvisor_core::kill(pid, created)).any(|r| r.is_err()) {
            let _ = unsafe { windows::Win32::System::Diagnostics::Debug::MessageBeep(MB_ICONWARNING) };
        }
        let _ = self.signal.send(Signal::Rescan);
    }

    fn run_new_task(&mut self) {
        self.modal += 1;
        shell::run_dialog(self.hwnd);
        self.modal -= 1;
        let _ = self.signal.send(Signal::Rescan);
    }

    /// Screen point for a menu: the mouse, or for a keyboard menu, the focused element.
    fn menu_at(&self, keyboard: bool) -> Option<POINT> {
        if !keyboard {
            return None;
        }
        let (x, y) = match self.focus {
            Some(Focus::Rows) => {
                let i = self.table.selected_index().unwrap_or(0) as f32;
                (self.layout_cols()[0].1 + 40.0, TOP + HEAD + (i - self.table.scroll + 1.0) * ROW)
            }
            Some(Focus::Header(k)) => (self.layout_cols().get(k).map_or(0.0, |c| c.1 + 8.0), TOP + HEAD),
            Some(Focus::Search) => (render::search(self.size().0).0 + 34.0, SEARCH_Y.1),
            _ => return None,
        };
        let s = dpi(self.hwnd) as f32 / 96.0;
        let mut p = POINT { x: (x * s) as i32, y: (y * s) as i32 };
        let _ = unsafe { ClientToScreen(self.hwnd, &mut p) };
        Some(p)
    }

    fn row_menu(&mut self, keyboard: bool) {
        let rows = self.end_rows();
        let Some(&first) = rows.first() else { return };
        let row = &self.table.rows[first];
        let end = match (rows.len(), &row.key) {
            (n, _) if n > 1 => format!("End {n} tasks"),
            (_, Key::Group(..)) => format!("End all {} {}", row.procs.len(), row.label),
            _ => "End task".to_owned(),
        };
        let has_path = self.cursor_proc().is_some_and(|p| !p.path.is_empty());
        let path_flags = if has_path { MF_STRING } else { MF_STRING | MF_GRAYED };
        let at = self.menu_at(keyboard);
        let cmd = self.popup(&[(MENU_END, MF_STRING, &end), (0, MF_SEPARATOR, ""), (MENU_OPEN, path_flags, "Open file location"), (MENU_COPY_PATH, path_flags, "Copy path\tCtrl+C"), (MENU_COPY_CMD, MF_STRING, "Copy command line")], at);
        if cmd == MENU_END {
            return self.end_task();
        }
        let Some(p) = self.cursor_proc() else { return };
        let (pid, path) = (p.pid, p.path.clone());
        match cmd {
            MENU_OPEN => shell::open_location(&path),
            MENU_COPY_PATH => shell::copy(self.hwnd, &path),
            MENU_COPY_CMD => shell::copy(self.hwnd, &suprvisor_core::command_line(pid).unwrap_or_default()),
            _ => {}
        }
    }

    /// Group by type, and which columns show.
    fn header_menu(&mut self, keyboard: bool) {
        let check = |on: bool| if on { MF_STRING | MF_CHECKED } else { MF_STRING };
        let mut items = vec![(MENU_GROUPED, check(self.table.grouped), "Group by type".to_owned()), (0, MF_SEPARATOR, String::new())];
        for (i, c) in NUMS.iter().enumerate() {
            items.push((MENU_COLUMN + i, check(!self.settings.hidden[i]), c.label().to_owned()));
        }
        items.push((0, MF_SEPARATOR, String::new()));
        items.push((MENU_RESET, MF_STRING, "Reset columns".to_owned()));
        let refs: Vec<(usize, MENU_ITEM_FLAGS, &str)> = items.iter().map(|(id, f, s)| (*id, *f, s.as_str())).collect();
        let at = self.menu_at(keyboard);
        match self.popup(&refs, at) {
            MENU_GROUPED => {
                self.table.grouped = !self.table.grouped;
                self.rebuild();
            }
            MENU_RESET => self.relayout(|a| {
                let s = &mut a.settings;
                (s.widths, s.order, s.hidden) = (settings::DEFAULT_WIDTHS, NUMS, [false; 6]);
            }),
            id if (MENU_COLUMN..MENU_COLUMN + 6).contains(&id) => self.relayout(|a| a.settings.hidden[id - MENU_COLUMN] ^= true),
            _ => return,
        }
        if let Some(Focus::Header(k)) = self.focus {
            self.focus = Some(Focus::Header(k.min(self.layout_cols().len() - 1)));
        }
        self.save_later();
    }

    /// The search box's menu, like a Windows edit control's.
    fn edit_menu(&mut self, keyboard: bool) {
        let some = |on: bool| if on { MF_STRING } else { MF_STRING | MF_GRAYED };
        let sel = !self.edit.selected().is_empty();
        let at = self.menu_at(keyboard);
        let cmd = self.popup(
            &[
                (MENU_UNDO, MF_STRING, "Undo\tCtrl+Z"),
                (0, MF_SEPARATOR, ""),
                (MENU_CUT, some(sel), "Cut\tCtrl+X"),
                (MENU_COPY, some(sel), "Copy\tCtrl+C"),
                (MENU_PASTE, MF_STRING, "Paste\tCtrl+V"),
                (MENU_DELETE, some(sel), "Delete\tDel"),
                (0, MF_SEPARATOR, ""),
                (MENU_SELECT_ALL, some(!self.edit.text.is_empty()), "Select all\tCtrl+A"),
            ],
            at,
        );
        let key = match cmd {
            MENU_UNDO => VK_Z,
            MENU_CUT => VK_X,
            MENU_COPY => VK_C,
            MENU_PASTE => VK_V,
            MENU_SELECT_ALL => VK_A,
            MENU_DELETE => {
                self.edit_key(VK_DELETE, false, false);
                return;
            }
            _ => return,
        };
        self.edit_key(key, true, false);
    }

    /// A native popup menu at `at` (screen pixels) or the cursor. Returns the chosen id, 0 for none.
    fn popup(&mut self, items: &[(usize, MENU_ITEM_FLAGS, &str)], at: Option<POINT>) -> usize {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return 0 };
            for &(id, flags, text) in items {
                let _ = AppendMenuW(menu, flags, id, &HSTRING::from(text));
            }
            let p = at.unwrap_or_else(|| {
                let mut p = POINT::default();
                let _ = GetCursorPos(&mut p);
                p
            });
            let _ = SetForegroundWindow(self.hwnd); // so the menu closes when clicking elsewhere
            let id = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_RETURNCMD, p.x, p.y, Some(0), self.hwnd, None).0 as usize;
            let _ = DestroyMenu(menu);
            id
        }
    }

    /// The filter text changed: the list starts again at the top.
    fn filter_changed(&mut self) {
        self.caret_input();
        if self.table.filter == self.edit.text {
            return;
        }
        self.table.filter = self.edit.text.clone();
        (self.scroll_to, self.table.scroll) = (0.0, 0.0);
        if self.performance {
            self.set_tab(false); // rebuilds
        } else {
            self.rebuild();
        }
    }

    /// Text box keys. Returns whether the key was one.
    fn edit_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
        let e = &mut self.edit;
        match vk {
            VK_LEFT | VK_RIGHT => e.arrow(vk == VK_RIGHT, ctrl, shift),
            VK_HOME => e.move_to(0, shift),
            VK_END => e.move_to(e.text.len(), shift),
            VK_BACK => {
                e.delete(false, ctrl);
            }
            VK_DELETE if shift && !e.selected().is_empty() => {
                // Shift+Delete cuts, as in Windows text boxes.
                shell::copy(self.hwnd, e.selected());
                e.delete(true, false);
            }
            VK_DELETE => {
                e.delete(true, ctrl);
            }
            VK_INSERT if ctrl && !e.selected().is_empty() => shell::copy(self.hwnd, e.selected()),
            VK_INSERT if shift => {
                if let Some(s) = shell::paste(self.hwnd) {
                    e.insert(&s.replace(['\r', '\n'], " "), false);
                }
            }
            VK_A if ctrl => e.select_all(),
            VK_C if ctrl => {
                if !e.selected().is_empty() {
                    shell::copy(self.hwnd, e.selected());
                }
            }
            VK_X if ctrl => {
                if !e.selected().is_empty() {
                    shell::copy(self.hwnd, e.selected());
                    e.delete(true, false);
                }
            }
            VK_V if ctrl => {
                if let Some(s) = shell::paste(self.hwnd) {
                    e.insert(s.trim_end_matches(['\r', '\n']).replace(['\r', '\n'], " ").as_str(), false);
                }
            }
            VK_Z if ctrl => {
                e.undo();
            }
            _ => return false,
        }
        self.filter_changed();
        true
    }

    /// Tab stops, in order. The navigation pane, the column headers and the rows are one stop each;
    /// arrow keys move inside them.
    fn tab_stops(&self) -> Vec<Focus> {
        let mut v = vec![Focus::Search, Focus::Nav(if self.performance { 2 } else { 1 }), Focus::Command(0)];
        if !self.performance {
            if !self.end_rows().is_empty() {
                v.push(Focus::Command(1));
            }
            let sorted = self.layout_cols().iter().position(|c| c.0 == self.table.sort).unwrap_or(0);
            v.extend([Focus::Header(sorted), Focus::Rows]);
        }
        v
    }

    fn tab(&mut self, back: bool) {
        let stops = self.tab_stops();
        let same = |a: &Focus, b: &Focus| std::mem::discriminant(a) == std::mem::discriminant(b) && (!matches!(a, Focus::Command(_)) || a == b);
        let n = stops.len();
        let next = match self.focus.and_then(|f| stops.iter().position(|s| same(s, &f))) {
            Some(i) if back => (i + n - 1) % n,
            Some(i) => (i + 1) % n,
            None if back => n - 1,
            None => 0,
        };
        if stops[next] == Focus::Rows {
            return self.focus_rows();
        }
        self.set_focus(Some(stops[next]), true);
    }

    /// Focus to the rows: the cursor row, or the first process row.
    fn focus_rows(&mut self) {
        self.set_focus(Some(Focus::Rows), true);
        match self.table.selected_index() {
            Some(i) => {
                if self.table.marked.is_empty() {
                    self.select(i);
                }
                self.ensure_visible(i);
            }
            None => {
                let first = self.table.rows.iter().position(|r| !matches!(r.key, Key::Section(_))).unwrap_or(0);
                self.select(first);
            }
        }
    }

    fn activate_nav(&mut self, k: usize) {
        match k {
            0 => {
                self.nav_open = !self.nav_open;
                self.dirty = true;
                self.save_later();
            }
            3 => ifeo::open_native(),
            k => self.set_tab(k == 2),
        }
    }

    fn activate_command(&mut self, k: usize) {
        match k {
            0 => self.run_new_task(),
            _ => self.end_task(),
        }
    }

    fn sort_by(&mut self, c: Col) {
        self.table.sort_by(c);
        self.changed();
        self.save_later();
    }

    /// Moves a column one place left or right (Ctrl+Left/Right on a header).
    fn move_column(&mut self, c: Col, right: bool) {
        let Some(i) = self.settings.order.iter().position(|&k| k == c) else { return };
        // The neighbor among the shown columns; hidden ones in between keep their places.
        let shown = |a: &App, j: usize| !a.settings.hidden[a.settings.order[j].num().unwrap()];
        let j = if right { (i + 1..6).find(|&j| shown(self, j)) } else { (0..i).rev().find(|&j| shown(self, j)) };
        if let Some(j) = j {
            self.relayout(|a| a.settings.order.swap(i, j));
            self.save_later();
        }
    }

    /// Fits a column's width to its widest cell (double-click on its right separator).
    fn autofit(&mut self, c: Col) {
        let Some(n) = c.num() else { return };
        let mut w = self.renderer.measure(&render::total(&self.snap.system, c), true).max(self.renderer.measure(c.label(), false));
        for row in &self.table.rows {
            if !matches!(row.key, Key::Section(_)) {
                w = w.max(self.renderer.measure(&render::cell(row, c), false));
            }
        }
        let w = (w + 20.0).ceil().clamp(render::MIN_WIDTH, 600.0);
        self.relayout(|a| a.settings.widths[n] = w);
        self.save_later();
    }

    /// While a column is dragged: it swaps places with a neighbor once its middle passes the
    /// neighbor's middle. The neighbor slides over.
    fn reorder_step(&mut self, c: Col, x: f32, grab: f32) {
        loop {
            let cols = self.layout_cols();
            let Some(k) = cols.iter().position(|col| col.0 == c) else { return };
            let w = cols[k].2 - cols[k].1;
            let mid = x - grab + w / 2.0;
            let target = if k > 1 && mid < (cols[k - 1].1 + cols[k - 1].2) / 2.0 {
                cols[k - 1].0
            } else if k + 1 < cols.len() && mid > (cols[k + 1].1 + cols[k + 1].2) / 2.0 {
                cols[k + 1].0
            } else {
                return;
            };
            let o = &self.settings.order;
            let (i, j) = (o.iter().position(|&k| k == c).unwrap(), o.iter().position(|&k| k == target).unwrap());
            self.relayout(|a| a.settings.order.swap(i, j));
        }
    }

    fn key(&mut self, vk: VIRTUAL_KEY) -> bool {
        let (ctrl, shift) = (ctrl(), shift());
        // Window-wide shortcuts.
        match vk {
            VK_TAB if ctrl => self.set_tab(!self.performance),
            VK_1 | VK_2 if ctrl => self.set_tab(vk == VK_2),
            VK_F if ctrl => {
                self.set_focus(Some(Focus::Search), true);
                self.edit.select_all();
            }
            VK_W if ctrl => self.close(),
            VK_N if ctrl => self.run_new_task(),
            VK_F5 => {
                let _ = self.signal.send(Signal::Rescan);
            }
            VK_TAB => self.tab(shift),
            VK_ESCAPE if !matches!(self.drag, Drag::None) => {
                if matches!(self.drag, Drag::Reorder(..) | Drag::Column { .. } | Drag::Thumb(_) | Drag::Select { .. } | Drag::Text) {
                    let _ = unsafe { ReleaseCapture() };
                }
                self.drag = Drag::None;
                self.dirty = true;
            }
            VK_ESCAPE if !self.edit.text.is_empty() => {
                self.edit.clear();
                self.filter_changed();
            }
            VK_ESCAPE => self.close(),
            _ => return self.focus_key(vk, ctrl, shift),
        }
        true
    }

    /// Keys for the focused element.
    fn focus_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
        let activate = vk == VK_RETURN || vk == VK_SPACE;
        match self.focus {
            Some(Focus::Search) => match vk {
                VK_DOWN | VK_NEXT | VK_RETURN if !self.performance => self.focus_rows(),
                _ => return self.edit_key(vk, ctrl, shift),
            },
            Some(Focus::Nav(k)) => match vk {
                VK_UP => self.set_focus(Some(Focus::Nav(k.saturating_sub(1))), true),
                VK_DOWN => self.set_focus(Some(Focus::Nav((k + 1).min(render::NAV_ITEMS - 1))), true),
                _ if activate => self.activate_nav(k),
                _ => return false,
            },
            Some(Focus::Command(k)) => match vk {
                VK_LEFT | VK_RIGHT => {
                    let n = render::commands(0.0, self.performance).len();
                    let k = if vk == VK_LEFT { k.saturating_sub(1) } else { (k + 1).min(n - 1) };
                    if k == 0 || !self.end_rows().is_empty() {
                        self.set_focus(Some(Focus::Command(k)), true);
                    }
                }
                _ if activate => self.activate_command(k),
                _ => return false,
            },
            Some(Focus::Header(k)) => {
                let cols = self.layout_cols();
                let k = k.min(cols.len() - 1);
                let c = cols[k].0;
                match vk {
                    VK_LEFT | VK_RIGHT if ctrl => {
                        self.move_column(c, vk == VK_RIGHT);
                        let k = self.layout_cols().iter().position(|col| col.0 == c).unwrap_or(k);
                        self.set_focus(Some(Focus::Header(k)), true);
                    }
                    VK_LEFT => self.set_focus(Some(Focus::Header(k.saturating_sub(1))), true),
                    VK_RIGHT => self.set_focus(Some(Focus::Header((k + 1).min(cols.len() - 1))), true),
                    VK_HOME => self.set_focus(Some(Focus::Header(0)), true),
                    VK_END => self.set_focus(Some(Focus::Header(cols.len() - 1)), true),
                    VK_DOWN => self.focus_rows(),
                    _ if activate => self.sort_by(c),
                    _ => return false,
                }
            }
            Some(Focus::Rows) if !self.performance => return self.row_key(vk, ctrl, shift),
            _ => match vk {
                // Nothing focused yet: the arrows start in the rows, as in Task Manager.
                VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT | VK_HOME | VK_END if !self.performance => self.focus_rows(),
                _ => return false,
            },
        }
        true
    }

    fn row_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
        let cur = self.table.selected_index();
        let last = self.table.rows.len().saturating_sub(1);
        let page = self.page();
        let to = match vk {
            VK_UP if cur == Some(0) && !shift && !ctrl => {
                // Up from the first row goes back to the search box.
                self.set_focus(Some(Focus::Search), true);
                return true;
            }
            VK_UP => Some(cur.map_or(0, |i| i.saturating_sub(1))),
            VK_DOWN => Some(cur.map_or(0, |i| (i + 1).min(last))),
            VK_PRIOR => Some(cur.map_or(0, |i| i.saturating_sub(page))),
            VK_NEXT => Some(cur.map_or(0, |i| (i + page).min(last))),
            VK_HOME => Some(0),
            VK_END => Some(last),
            _ => None,
        };
        if let Some(i) = to {
            self.focus_visible = true;
            match (shift, ctrl) {
                (true, _) => self.extend(i),
                (false, true) => self.move_cursor(i),
                _ => self.select(i),
            }
            return true;
        }
        match vk {
            VK_SPACE => {
                if let Some(i) = cur {
                    if ctrl { self.toggle_mark(i) } else { self.select(i) }
                }
            }
            VK_A if ctrl => {
                self.table.mark_range(0, last, &HashSet::new());
                self.dirty = true;
            }
            VK_LEFT | VK_RIGHT | VK_RETURN => {
                let Some(i) = cur else { return true };
                let row = &self.table.rows[i];
                match vk {
                    VK_LEFT if !row.expanded => {
                        // Go to the parent row.
                        let d = row.depth;
                        if let Some(p) = self.table.rows[..i].iter().rposition(|r| r.depth < d) {
                            self.select(p);
                        }
                    }
                    VK_RIGHT if row.expanded => self.select((i + 1).min(last)),
                    VK_RIGHT if !row.expandable => {}
                    _ => {
                        self.table.toggle(i);
                        self.changed();
                    }
                }
            }
            VK_DELETE => self.end_task(),
            VK_C if ctrl => {
                if let Some(p) = self.cursor_proc() {
                    let path = p.path.clone();
                    shell::copy(self.hwnd, &path);
                }
            }
            _ => return false,
        }
        true
    }

    /// Caret position in the search text under client x.
    fn search_pos(&mut self, x: f32) -> usize {
        let (l, _) = render::search_text(self.size().0, !self.edit.text.is_empty());
        let u = self.renderer.search_index(&self.edit.text, x - l);
        self.edit.from_utf16(u)
    }

    fn mouse_down(&mut self, x: f32, y: f32, double: bool) {
        let hit = self.hit(x, y);
        let (ctrl, shift) = (ctrl(), shift());
        match hit {
            Hit::Search => {}
            Hit::Header(_) | Hit::Edge(_) | Hit::Row(..) => self.set_focus(Some(if matches!(hit, Hit::Row(..)) { Focus::Rows } else { Focus::Header(0) }), false),
            _ => self.set_focus(None, false),
        }
        match hit {
            Hit::Search => {
                self.set_focus(Some(Focus::Search), false);
                let i = self.search_pos(x);
                if double {
                    self.edit.word_at(i);
                } else {
                    self.edit.move_to(i, shift);
                }
                self.drag = Drag::Text;
                unsafe { SetCapture(self.hwnd) };
                self.caret_input();
            }
            Hit::Edge(k) if double => {
                // Double-click on a separator fits the column on its left (CPU for Name|CPU).
                let cols = self.layout_cols();
                self.autofit(if k == 1 { cols[1].0 } else { cols[k - 1].0 });
            }
            Hit::Edge(k) => {
                let cols = self.layout_cols();
                let (left, right) = (cols[k - 1].0.num(), cols[k].0.num().unwrap());
                self.drag = Drag::Column { left, right, x, start: self.settings.widths, name: cols[0].2 - cols[0].1 };
                unsafe { SetCapture(self.hwnd) };
            }
            // No capture for a header press: a click is the common case, and SetCapture and
            // ReleaseCapture go into the window manager, which can make the thread wait when
            // other processes hold it. A release outside the window does not sort.
            Hit::Header(c) => {
                if let Some(k) = self.layout_cols().iter().position(|col| col.0 == c) {
                    self.focus = Some(Focus::Header(k));
                }
                self.drag = Drag::Header(c, x);
            }
            Hit::Row(i, chevron) => {
                if chevron || double {
                    self.select(i);
                    self.table.toggle(i);
                    self.changed();
                } else if ctrl {
                    self.toggle_mark(i);
                } else if shift {
                    self.extend(i);
                } else {
                    self.select(i);
                }
                if !chevron && !double {
                    self.drag = Drag::Row(i, y);
                }
            }
            Hit::Thumb => {
                if let Some((y0, _)) = render::thumb(self.size().1, self.table.rows.len(), self.table.scroll) {
                    self.drag = Drag::Thumb(y - y0);
                    unsafe { SetCapture(self.hwnd) };
                }
            }
            Hit::Nav(k) => self.activate_nav(k),
            Hit::Command(k) if k == 0 || !self.end_rows().is_empty() => self.activate_command(k),
            Hit::SearchClear => {
                self.edit.clear();
                self.set_focus(Some(Focus::Search), false);
                self.filter_changed();
            }
            Hit::Track => {
                let up = render::thumb(self.size().1, self.table.rows.len(), self.table.scroll).is_some_and(|(y0, _)| y < y0);
                let page = self.page() as f32;
                self.scroll_by(if up { -page } else { page });
            }
            _ => {}
        }
    }

    fn mouse_move(&mut self, x: f32, y: f32) {
        self.mouse = Some((x, y));
        match self.drag {
            Drag::Column { left, right, x: x0, start, name } => {
                self.settings.widths = render::resize(start, left, right, x - x0, name);
                self.dirty = true;
            }
            Drag::Thumb(grab) => {
                let (_, h) = self.size();
                let rows = self.table.rows.len();
                if let Some((y0, y1)) = render::thumb(h, rows, 0.0) {
                    let span = h - TOP - HEAD - (y1 - y0);
                    let max = rows.saturating_sub(self.page()) as f32;
                    self.scroll_to = ((y - grab - y0) / span * max).clamp(0.0, max);
                    self.table.scroll = self.scroll_to;
                    self.changed();
                }
            }
            Drag::Header(c, x0) if c != Col::Name && (x - x0).abs() > 5.0 => {
                let col = self.layout_cols().into_iter().find(|col| col.0 == c);
                if let Some((_, l, _)) = col {
                    self.drag = Drag::Reorder(c, x0 - l);
                    self.set_hot(Hot::None);
                    unsafe { SetCapture(self.hwnd) };
                    self.reorder_step(c, x, x0 - l);
                }
            }
            Drag::Reorder(c, grab) => {
                self.reorder_step(c, x, grab);
                self.dirty = true;
            }
            Drag::Row(i, y0) if (y - y0).abs() > 4.0 => {
                let base = if ctrl() { self.table.marked.clone() } else { HashSet::new() };
                unsafe { SetCapture(self.hwnd) };
                self.select_drag(i, y, &base);
                self.drag = Drag::Select { anchor: i, base };
            }
            Drag::Select { .. } => {
                self.autoscroll(0.0);
                self.dirty = true; // the frame loop scrolls while the mouse is outside the rows
            }
            Drag::Text => {
                let i = self.search_pos(x);
                self.edit.move_to(i, true);
                self.caret_input();
            }
            Drag::Header(..) | Drag::Row(..) => {}
            Drag::None => {
                let hit = self.hit(x, y);
                self.set_hot(match hit {
                    Hit::Header(c) => Hot::Header(c),
                    Hit::Nav(k) => Hot::Nav(k),
                    Hit::Search => Hot::Search,
                    Hit::SearchClear => Hot::SearchClear,
                    Hit::Command(k) => Hot::Command(k),
                    _ => Hot::None,
                });
                let hover = match hit {
                    Hit::Row(i, _) => Some(i),
                    _ => None,
                };
                if hover != self.table.hover {
                    self.table.hover = hover;
                    self.dirty = true;
                }
                let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
                let _ = unsafe { TrackMouseEvent(&mut t) };
            }
        }
    }

    fn mouse_up(&mut self, x: f32, y: f32) {
        match std::mem::replace(&mut self.drag, Drag::None) {
            Drag::Column { .. } => self.save_later(),
            Drag::Header(c, _) if matches!(self.hit(x, y), Hit::Header(h) if h == c) => self.sort_by(c),
            Drag::Reorder(c, grab) => {
                // Drop: the column slides from the mouse into its place.
                if let Some((_, l, _)) = self.layout_cols().into_iter().find(|col| col.0 == c) {
                    self.col_off[ci(c)] = x - grab - l;
                }
                self.dirty = true;
                self.save_later();
            }
            _ => {}
        }
        if unsafe { GetCapture() } == self.hwnd {
            let _ = unsafe { ReleaseCapture() };
        }
    }

    /// The menu for where the mouse is (`keyboard` false) or for the focused element.
    fn context_menu(&mut self, hit: Option<Hit>) {
        let keyboard = hit.is_none();
        let target = match hit {
            Some(Hit::Row(i, _)) => {
                if !self.table.marked.contains(&self.table.rows[i].key) {
                    self.select(i);
                }
                self.set_focus(Some(Focus::Rows), false);
                Some(Focus::Rows)
            }
            Some(Hit::Header(_) | Hit::Edge(_)) => Some(Focus::Header(0)),
            Some(Hit::Search) | Some(Hit::SearchClear) => {
                self.set_focus(Some(Focus::Search), false);
                Some(Focus::Search)
            }
            Some(_) => None,
            None => self.focus,
        };
        match target {
            Some(Focus::Rows) if !self.performance => self.row_menu(keyboard),
            Some(Focus::Header(_)) if !self.performance => self.header_menu(keyboard),
            Some(Focus::Search) => self.edit_menu(keyboard),
            _ => {}
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let t = Instant::now();
    let c0 = thread_kcycles();
    let r = handle(hwnd, msg, wp, lp);
    let name = match msg {
        WM_PAINT => "WM_PAINT",
        WM_MOUSEMOVE => "WM_MOUSEMOVE",
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => "WM_LBUTTONDOWN",
        WM_LBUTTONUP => "WM_LBUTTONUP",
        WM_MOUSEWHEEL => "WM_MOUSEWHEEL",
        WM_KEYDOWN => "WM_KEYDOWN",
        WM_CHAR => "WM_CHAR",
        WM_SIZE => "WM_SIZE",
        WM_APP_DATA => "WM_APP_DATA",
        WM_APP_SHOW | WM_APP_HOTKEY => "WM_APP_SHOW",
        _ => return r,
    };
    trace::record(name, t, thread_kcycles() - c0);
    if msg == WM_DESTROY {
        trace::flush();
    }
    r
}

/// CPU cycles of this thread, in thousands: the trace's count for a message, so a slow handler
/// shows whether it ran or waited.
fn thread_kcycles() -> u64 {
    let mut c = 0;
    let _ = unsafe { windows::Win32::System::WindowsProgramming::QueryThreadCycleTime(GetCurrentThread(), &mut c) };
    c / 1000
}

fn handle(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = unsafe { &*(lp.0 as *const CREATESTRUCTW) };
        let a = cs.lpCreateParams as *mut App;
        unsafe {
            (*a).hwnd = hwnd;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, a as isize);
        }
    }
    let Some(a) = app(hwnd) else { return unsafe { DefWindowProcW(hwnd, msg, wp, lp) } };
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe { BeginPaint(hwnd, &mut ps) };
            let _ = unsafe { EndPaint(hwnd, &ps) };
            if a.modal > 0 {
                a.paint();
            } else {
                a.dirty = true; // the message loop draws it once the input is handled
            }
            LRESULT(0)
        }
        WM_ENTERSIZEMOVE | WM_ENTERMENULOOP => {
            a.modal += 1;
            LRESULT(0)
        }
        WM_EXITSIZEMOVE | WM_EXITMENULOOP => {
            a.modal = a.modal.saturating_sub(1);
            LRESULT(0)
        }
        WM_TIMER => {
            match wp.0 {
                TIMER_SAVE if a.save_due.is_none_or(|t| Instant::now() >= t) => {
                    a.save_due = None;
                    let _ = unsafe { KillTimer(Some(hwnd), TIMER_SAVE) };
                    a.save();
                }
                TIMER_CARET => {
                    if a.focus == Some(Focus::Search) && a.caret_at.elapsed() < CARET_BLINK_FOR {
                        a.caret_on = !a.caret_on;
                    } else {
                        a.caret_on = true;
                        a.caret_timer = false;
                        let _ = unsafe { KillTimer(Some(hwnd), TIMER_CARET) };
                    }
                    a.dirty = true;
                    a.paint_in_modal();
                }
                TIMER_SHOW => {
                    let _ = unsafe { KillTimer(Some(hwnd), TIMER_SHOW) };
                    if let Some(activate) = a.pending_show.take() {
                        a.got_data = true; // no list in time: show without one
                        a.show_with(activate);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1), // Direct2D paints everything
        WM_SIZE => {
            a.renderer.resize((lp.0 & 0xFFFF) as u32, ((lp.0 >> 16) & 0xFFFF) as u32);
            // WM_SIZE also comes at creation, while the window is still hidden.
            let visible = wp.0 as u32 != SIZE_MINIMIZED && unsafe { IsWindowVisible(hwnd) }.as_bool();
            let _ = a.signal.send(Signal::Visible(visible));
            a.changed();
            if visible {
                a.nav_pill = a.pill_target(); // the bottom item moves with the height
                a.paint(); // the frame must match the new size now, also during a live resize
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let r = unsafe { &*(lp.0 as *const RECT) };
            a.renderer.set_dpi((wp.0 & 0xFFFF) as u32);
            let _ = unsafe { SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE) };
            LRESULT(0)
        }
        WM_SHOWWINDOW => {
            let _ = a.signal.send(Signal::Visible(wp.0 != 0));
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_ACTIVATE => {
            a.active = (wp.0 & 0xFFFF) as u32 != WA_INACTIVE;
            a.dirty = true;
            LRESULT(0)
        }
        WM_APP_DATA => {
            a.load();
            if let Some(activate) = a.pending_show.take() {
                let _ = unsafe { KillTimer(Some(hwnd), TIMER_SHOW) };
                a.show_with(activate);
            }
            LRESULT(0)
        }
        WM_APP_ICONS => {
            a.snap.icons = a.data.lock().unwrap().icons.clone();
            a.dirty = true;
            a.paint_in_modal();
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = a.point(lp);
            a.mouse_move(x, y);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if matches!(a.drag, Drag::Header(..) | Drag::Row(..)) {
                a.drag = Drag::None; // pressed without capture: the release will not come here
            }
            a.mouse = None;
            a.table.hover = None;
            a.hot = Hot::None;
            a.dirty = true;
            LRESULT(0)
        }
        // The title bar is client area (WM_NCCALCSIZE): Windows still moves, snaps and resizes the
        // window from the codes WM_NCHITTEST returns, and shows Snap Layouts over HTMAXBUTTON.
        WM_NCCALCSIZE if wp.0 != 0 => {
            let p = unsafe { &mut *(lp.0 as *mut NCCALCSIZE_PARAMS) };
            let top = p.rgrc[0].top;
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }; // side and bottom borders
            // A maximized window hangs over the screen edge by its border: keep that part hidden.
            p.rgrc[0].top = top + if unsafe { IsZoomed(hwnd) }.as_bool() { frame(hwnd) } else { 0 };
            LRESULT(0)
        }
        WM_NCHITTEST => {
            let d = unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
            if d.0 as u32 != HTCLIENT {
                return d;
            }
            let mut p = POINT { x: lp.0 as i16 as i32, y: (lp.0 >> 16) as i16 as i32 };
            let _ = unsafe { ScreenToClient(hwnd, &mut p) };
            let maximized = unsafe { IsZoomed(hwnd) }.as_bool();
            if !maximized && p.y < frame(hwnd) {
                return LRESULT(HTTOP as isize);
            }
            let s = 96.0 / dpi(hwnd) as f32;
            LRESULT(match a.hit(p.x as f32 * s, p.y as f32 * s) {
                Hit::Caption(k) => [HTMINBUTTON, HTMAXBUTTON, HTCLOSE][k],
                Hit::Title => HTCAPTION,
                _ => HTCLIENT,
            } as isize)
        }
        WM_NCMOUSEMOVE => {
            a.set_hot(caption(wp).map_or(Hot::None, Hot::Caption));
            let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE | TME_NONCLIENT, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = unsafe { TrackMouseEvent(&mut t) };
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_NCMOUSELEAVE => {
            a.set_hot(Hot::None);
            LRESULT(0)
        }
        // Caption buttons act on release over the same button, like Windows' own. Default handling
        // would draw the old Windows buttons.
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if caption(wp).is_some() => {
            a.caption_down = wp.0 as u32;
            LRESULT(0)
        }
        WM_NCLBUTTONUP if caption(wp).is_some() => {
            let down = std::mem::take(&mut a.caption_down);
            if down == wp.0 as u32 {
                a.set_hot(Hot::None);
                let command = match down {
                    HTMINBUTTON => SC_MINIMIZE,
                    HTMAXBUTTON if unsafe { IsZoomed(hwnd) }.as_bool() => SC_RESTORE,
                    HTMAXBUTTON => SC_MAXIMIZE,
                    _ => SC_CLOSE,
                };
                let _ = unsafe { PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(command as usize), LPARAM(0)) };
            }
            LRESULT(0)
        }
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            let mut p = POINT::default();
            let _ = unsafe { GetCursorPos(&mut p) };
            let _ = unsafe { ScreenToClient(hwnd, &mut p) };
            let s = 96.0 / dpi(hwnd) as f32;
            let cursor = match a.hit(p.x as f32 * s, p.y as f32 * s) {
                _ if matches!(a.drag, Drag::Column { .. }) => IDC_SIZEWE,
                _ if matches!(a.drag, Drag::Text) => IDC_IBEAM,
                Hit::Edge(_) => IDC_SIZEWE,
                Hit::Search => IDC_IBEAM,
                _ => IDC_ARROW,
            };
            let _ = unsafe { SetCursor(LoadCursorW(None, cursor).ok()) };
            LRESULT(1)
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let (x, y) = a.point(lp);
            a.mouse_down(x, y, msg == WM_LBUTTONDBLCLK);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = a.point(lp);
            a.mouse_up(x, y);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = a.point(lp);
            let hit = a.hit(x, y);
            a.context_menu(Some(hit));
            LRESULT(0)
        }
        // Shift+F10 and the menu key: the focused element's menu. lParam -1 marks the keyboard.
        WM_CONTEXTMENU if lp.0 == -1 => {
            a.focus_visible = true;
            a.context_menu(None);
            LRESULT(0)
        }
        WM_MOUSEWHEEL if a.performance => LRESULT(0),
        WM_MOUSEWHEEL => {
            // Notches of 120; touchpads send less per message, and the rows follow smoothly.
            let delta = (wp.0 >> 16) as u16 as i16 as f32;
            let mut lines = 0u32;
            let _ = unsafe { SystemParametersInfoW(SPI_GETWHEELSCROLLLINES, 0, Some((&raw mut lines).cast()), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
            let lines = if lines == 0 || lines > 100 { a.page() as f32 } else { lines as f32 };
            a.scroll_by(-delta / 120.0 * lines);
            if let Drag::Select { .. } = a.drag {
                a.autoscroll(0.0);
            }
            LRESULT(0)
        }
        WM_KEYDOWN if a.key(VIRTUAL_KEY(wp.0 as u16)) => {
            a.paint_in_modal();
            LRESULT(0)
        }
        WM_CHAR => {
            // Typing anywhere goes to the search box and filters by name, window title or PID.
            // Ctrl alone makes control codes; Ctrl+Alt (AltGr) types characters like @ and #.
            let typed = char::from_u32(wp.0 as u32).filter(|c| !c.is_control() && !(ctrl() && !down(VK_MENU)));
            // Space is a key (select, press) outside the search box.
            if let Some(c) = typed.filter(|&c| c != ' ' || a.focus == Some(Focus::Search)) {
                if a.focus != Some(Focus::Search) {
                    a.set_focus(Some(Focus::Search), false);
                    a.edit.move_to(a.edit.text.len(), false);
                }
                a.edit.insert(c.encode_utf8(&mut [0; 4]), true);
                a.filter_changed();
            }
            LRESULT(0)
        }
        WM_APP_SHOW | WM_APP_HOTKEY => {
            a.show();
            LRESULT(0)
        }
        WM_APP_HIDE => {
            a.hide();
            LRESULT(0)
        }
        WM_APP_TEST_MODS if test() => {
            TEST_MODS.store(wp.0, std::sync::atomic::Ordering::Relaxed);
            LRESULT(0)
        }
        WM_APP_TEST_SNAP if test() => {
            a.active = true; // as if focused: caret and focus rings show
            a.renderer.snap = Some(std::env::temp_dir().join("suprvisor-test.bmp").to_string_lossy().into_owned());
            a.dirty = true;
            LRESULT(0)
        }
        WM_CLOSE | WM_APP_EXIT => {
            a.close(); // closing ends the process
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            a.theme = theme::Theme::current();
            style_window(hwnd, a.theme.dark);
            a.dirty = true;
            LRESULT(0)
        }
        WM_DESTROY => {
            a.save();
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(a as *mut App));
                PostQuitMessage(0);
                trace::flush();
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
