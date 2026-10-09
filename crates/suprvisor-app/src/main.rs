//! Suprvisor: a resident task manager. One process stays in the tray with its window hidden.
//! Ctrl+Shift+Esc shows it (keyboard hook, hotkey.rs), and so does running suprvisor.exe again.
//!   suprvisor.exe                 show (start the resident process if none runs)
//!   suprvisor.exe --resident      start hidden (the logon task)
//!   suprvisor.exe --hide          hide
//!   suprvisor.exe --exit          quit the resident process
//!   suprvisor.exe --ifeo ...      run by Windows in place of taskmgr.exe (ifeo.rs)
//!   suprvisor.exe --real-taskmgr  start the real Task Manager (the fallback task)
#![windows_subsystem = "windows"]

mod hotkey;
mod ifeo;
mod render;
mod settings;
mod shell;
mod table;
mod theme;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use suprvisor_core::{Collector, Proc, System};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT, ScreenToClient, UpdateWindow};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, Result, w};

use render::{BAR, HEAD, Hot, ROW, SEARCH_Y, TITLE, TOP};
use table::{COLS, Key, Table};

const CLASS: PCWSTR = w!("Suprvisor");
const WM_APP_SHOW: u32 = WM_APP + 1;
const WM_APP_HIDE: u32 = WM_APP + 2;
const WM_APP_EXIT: u32 = WM_APP + 3;
const WM_APP_DATA: u32 = WM_APP + 4; // the collector thread has a new snapshot
const WM_APP_TRAY: u32 = WM_APP + 5;
const WM_APP_HOTKEY: u32 = WM_APP + 6; // Ctrl+Shift+Esc, from the keyboard hook
const MENU_SHOW: usize = 1;
const MENU_EXIT: usize = 2;
const MENU_END: usize = 10;
const MENU_OPEN: usize = 11;
const MENU_COPY_PATH: usize = 12;
const MENU_COPY_CMD: usize = 13;
const MENU_GROUPED: usize = 14;

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
    Header(table::Col),      // pressed: sorts on release over the same header, like a click
    Column(usize, f32, f32), // numeric column, mouse x and width at the start
    Thumb(f32),              // mouse y minus the thumb top
}

struct App {
    hwnd: HWND,
    renderer: render::Renderer,
    theme: theme::Theme,
    data: Arc<Mutex<Snapshot>>,
    wanted: Arc<Mutex<Vec<(u32, i64)>>>, // processes under the visible rows: refreshed fast
    signal: mpsc::Sender<Signal>,
    snap: Snapshot,
    table: Table,
    settings: settings::Settings,
    performance: bool, // the Performance tab is open
    nav_open: bool,    // the navigation pane shows labels
    focus: bool,       // the search box shows the keyboard focus: typing anywhere filters
    hot: Hot,
    caption_down: u32, // caption button pressed (HTCLOSE...), 0 for none
    drag: Drag,
    taskbar_created: u32,
    shown: bool,
    exiting: bool,
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
    // A resident instance exists: signal it and leave. This process got the user's input, so it may
    // pass the right to take the foreground to the resident one.
    if let Ok(existing) = unsafe { FindWindowW(CLASS, None) } {
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

    // Like Task Manager: the window must answer when the machine is saturated.
    let _ = unsafe { SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS) };
    let instance = unsafe { GetModuleHandleW(None)? };
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION)? },
        lpszClassName: CLASS,
        ..Default::default()
    };
    unsafe { RegisterClassExW(&wc) };

    let (signal_tx, signal_rx) = mpsc::channel();
    let data = Arc::new(Mutex::new(Snapshot::default()));
    let wanted = Arc::new(Mutex::new(Vec::new()));
    let settings = settings::Settings::load();
    let (window, performance) = (settings.window, settings.performance);
    let app = Box::new(App {
        hwnd: HWND::default(),
        renderer: render::Renderer::new()?,
        theme: theme::Theme::current(),
        data: data.clone(),
        wanted: wanted.clone(),
        signal: signal_tx,
        snap: Snapshot::default(),
        table: Table::new(settings.sort, settings.desc, settings.grouped),
        performance: settings.performance,
        nav_open: settings.nav,
        focus: false,
        hot: Hot::None,
        caption_down: 0,
        settings,
        drag: Drag::None,
        taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
        shown: false,
        exiting: false,
    });
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP, // all content comes through DirectComposition (render.rs)
            CLASS,
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
    add_tray_icon(hwnd);
    spawn_collector(hwnd, data, wanted, signal_rx, !performance);
    hotkey::install(hwnd, WM_APP_HOTKEY);
    // The resident process runs elevated (logon task); suprvisor.exe started from Explorer, and the
    // IFEO stub, do not. UIPI drops their messages unless allowed here.
    for m in [WM_APP_SHOW, WM_APP_HIDE, WM_APP_EXIT] {
        let _ = unsafe { ChangeWindowMessageFilterEx(hwnd, m, MSGFLT_ALLOW, None) };
    }
    if !resident {
        if window.is_some_and(|(_, max)| max) {
            let _ = unsafe { ShowWindow(hwnd, SW_SHOWMAXIMIZED) };
        }
        if let Some(a) = self::app(hwnd) {
            a.show();
        }
    }

    let mut m = MSG::default();
    while unsafe { GetMessageW(&mut m, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
    }
    Ok(())
}

/// Collector thread. Hidden, or on the Performance tab: machine totals once per second (the graph
/// history moves once per second, and each repaint costs ~4 ms of CPU). With the process table on
/// screen: totals every 500 ms, and every 500 ms the processes
/// under the visible rows; every 3 s the process list, window titles, new icons; every 60 s the
/// kernel list (per-process disk I/O). See suprvisor-core.
/// Once at start, hidden or not, the full list and every icon (~1-2 s of work): the first show then
/// has rows at once, and its fresh list (the quick handle scan, ~20 ms) follows.
fn spawn_collector(hwnd: HWND, data: Arc<Mutex<Snapshot>>, wanted: Arc<Mutex<Vec<(u32, i64)>>>, signal: mpsc::Receiver<Signal>, table: bool) {
    const SCAN: Duration = Duration::from_secs(3);
    const FULL: Duration = Duration::from_secs(60);
    const HISTORY: usize = 60; // seconds on the Performance graphs
    let hwnd = hwnd.0 as usize; // HWND is not Send; the value is valid on any thread
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
        let (mut primed, mut quick) = (false, false); // quick: the next list skips the kernel list
        let (mut scanned, mut full, mut engines, mut sampled): (Option<Instant>, Option<Instant>, Option<Instant>, Option<Instant>) = (None, None, None, None);
        loop {
            match signal.recv_timeout(Duration::from_millis(if shown && table { 500 } else { 1000 })) {
                Ok(Signal::Visible(v)) => {
                    if v && !shown {
                        (scanned, quick) = (None, true); // the list is stale after a hidden period
                    }
                    shown = v;
                }
                Ok(Signal::Table(t)) => {
                    if t && !table {
                        (scanned, quick) = (None, true); // not listed while the Performance tab was open
                    }
                    table = t;
                }
                Ok(Signal::Rescan) => scanned = None,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            let listing = (shown && table) || !primed;
            tick += 1;
            if !listing {
            } else if scanned.is_none_or(|t| t.elapsed() >= SCAN) {
                let quick = std::mem::take(&mut quick) && primed;
                procs = if !quick && full.is_none_or(|t| t.elapsed() >= FULL) {
                    full = Some(Instant::now());
                    c.scan_full().unwrap_or_else(|_| c.scan())
                } else {
                    c.scan()
                };
                scanned = Some(Instant::now());
                index = procs.iter().enumerate().map(|(i, p)| ((p.pid, p.create_time), i)).collect();
                snap.titles = Arc::new(shell::window_titles());
                let mut icons = None;
                for p in &procs {
                    if !p.path.is_empty() && !tried.contains(&p.path) {
                        tried.insert(p.path.clone());
                        if let Some(i) = shell::icon(&p.path) {
                            icons.get_or_insert_with(|| (*snap.icons).clone()).insert(p.path.clone(), Arc::new(i));
                        }
                    }
                }
                if let Some(i) = icons {
                    snap.icons = Arc::new(i);
                }
            } else {
                // Disk I/O is slow to read (~150 k cycles per process): every 3rd tick.
                let io = tick % 3 == 0;
                let keys = wanted.lock().unwrap().clone(); // do not hold the lock: the UI thread writes it
                for k in &keys {
                    if let Some(&i) = index.get(k) {
                        c.refresh(std::slice::from_mut(&mut procs[i]), io);
                    }
                }
            }
            // Idle GPU engines cost ~10 ms to read: every 3 s while visible, every 60 s while hidden.
            let all = engines.is_none_or(|t| t.elapsed() >= if shown { SCAN } else { FULL });
            if all {
                engines = Some(Instant::now());
            }
            snap.system = c.system(all);
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
            *data.lock().unwrap() = snap.clone();
            if shown || !std::mem::replace(&mut primed, true) {
                let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_DATA, WPARAM(0), LPARAM(0)) };
            }
        }
    });
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

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut d = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_APP_TRAY,
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default(),
        ..Default::default()
    };
    for (i, c) in "Suprvisor".encode_utf16().enumerate() {
        d.szTip[i] = c;
    }
    d
}

fn add_tray_icon(hwnd: HWND) {
    let _ = unsafe { Shell_NotifyIconW(NIM_ADD, &tray_data(hwnd)) };
}


fn app<'a>(hwnd: HWND) -> Option<&'a mut App> {
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut App;
    unsafe { p.as_mut() }
}

fn dpi(hwnd: HWND) -> u32 {
    unsafe { GetDpiForWindow(hwnd) }
}

fn invalidate(hwnd: HWND) {
    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
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

fn ctrl() -> bool {
    (unsafe { GetKeyState(VK_CONTROL.0 as i32) }) < 0
}

enum Hit {
    None,
    Header(table::Col),
    Edge(usize), // left edge of a numeric column: drag to resize
    Row(usize, bool), // row, on its chevron
    Thumb,
    Track,
    Nav(usize),     // 0 the menu button, 1 Processes, 2 Performance
    Search,
    SearchClear,
    EndTask,
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

    fn hit(&self, x: f32, y: f32) -> Hit {
        let (w, h) = self.size();
        if y < TITLE {
            if let Some(k) = render::caption_at(x, y, w) {
                return Hit::Caption(k);
            }
            let (l, r) = render::search(w);
            return match (l..r).contains(&x) && (SEARCH_Y.0..SEARCH_Y.1).contains(&y) {
                true if !self.table.filter.is_empty() && x >= r - render::CLEAR => Hit::SearchClear,
                true => Hit::Search,
                false => Hit::Title,
            };
        }
        if let Some(k) = render::nav_at(x, y, self.nav_open) {
            return Hit::Nav(k);
        }
        let cl = render::nav_width(self.nav_open);
        if x < cl || self.performance {
            return Hit::None;
        }
        if y < TOP {
            let r = render::end_task(w);
            return if (r.left..r.right).contains(&x) && (r.top..r.bottom).contains(&y) { Hit::EndTask } else { Hit::None };
        }
        let rows_top = TOP + HEAD;
        if x >= w - BAR && y >= rows_top {
            return match render::thumb(h, self.table.rows.len(), self.table.scroll) {
                Some((y0, y1)) if (y0..y1).contains(&y) => Hit::Thumb,
                Some(_) => Hit::Track,
                None => Hit::None,
            };
        }
        let cols = render::columns(cl, w, &self.settings.widths);
        if (TOP..rows_top).contains(&y) {
            if let Some(i) = (1..7).find(|&i| (x - cols[i].0).abs() <= 4.0) {
                return Hit::Edge(i - 1);
            }
            return COLS.iter().zip(&cols).find(|(_, (x0, x1))| (*x0..*x1).contains(&x)).map_or(Hit::None, |(c, _)| Hit::Header(*c));
        }
        if y >= rows_top {
            let i = self.table.scroll + ((y - rows_top) / ROW) as usize;
            if let Some(row) = self.table.rows.get(i) {
                let cx = render::chevron_x(row, cols[0].0);
                return Hit::Row(i, row.expandable && (cx - 2.0..cx + 18.0).contains(&x));
            }
        }
        Hit::None
    }

    /// After the rows or the scroll position change: tell the collector what is on screen, repaint.
    fn changed(&mut self) {
        let max = self.table.rows.len().saturating_sub(self.page());
        self.table.scroll = self.table.scroll.min(max);
        let s = self.table.scroll;
        *self.wanted.lock().unwrap() = self.table.keys(s..s + self.page() + 1);
        invalidate(self.hwnd);
    }

    /// Takes the collector's latest snapshot.
    fn load(&mut self) {
        self.snap = self.data.lock().unwrap().clone();
        self.table.procs = self.snap.procs.clone();
        self.table.titles = self.snap.titles.clone();
        if self.performance {
            invalidate(self.hwnd);
        } else {
            self.rebuild();
        }
    }

    fn set_hot(&mut self, hot: Hot) {
        if hot != self.hot {
            self.hot = hot;
            invalidate(self.hwnd);
        }
    }

    fn set_focus(&mut self, focus: bool) {
        if focus != self.focus {
            self.focus = focus;
            invalidate(self.hwnd);
        }
    }

    /// A process or group row is selected: End task acts on it.
    fn can_end(&self) -> bool {
        !self.performance && self.table.selected_index().is_some_and(|i| !matches!(self.table.rows[i].key, Key::Section(_)))
    }

    fn rebuild(&mut self) {
        self.table.rebuild();
        self.changed();
    }

    fn scroll_by(&mut self, n: isize) {
        self.table.scroll = self.table.scroll.saturating_add_signed(n);
        self.changed();
    }

    fn select(&mut self, i: usize) {
        let Some(row) = self.table.rows.get(i) else { return };
        self.table.selected = Some(row.key.clone());
        let page = self.page();
        if i < self.table.scroll {
            self.table.scroll = i;
        } else if i >= self.table.scroll + page {
            self.table.scroll = i + 1 - page;
        }
        self.changed();
    }

    fn set_tab(&mut self, performance: bool) {
        if performance == self.performance {
            return;
        }
        self.performance = performance;
        let _ = self.signal.send(Signal::Table(!performance));
        if performance {
            invalidate(self.hwnd);
        } else {
            self.rebuild();
        }
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
        if !self.shown {
            self.load(); // the collector kept the totals current while hidden, without telling the UI
        }
        unsafe {
            // SW_SHOW leaves a minimized window minimized, and the foreground request then only
            // flashes its taskbar button.
            let _ = ShowWindow(self.hwnd, if IsIconic(self.hwnd).as_bool() { SW_RESTORE } else { SW_SHOW });
            if !SetForegroundWindow(self.hwnd).as_bool() || GetForegroundWindow() != self.hwnd {
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
        self.focus = false;
        self.save();
        let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        self.shown = false;
        let _ = self.signal.send(Signal::Visible(false));
    }

    fn selected_proc(&self) -> Option<&Proc> {
        let row = &self.table.rows[self.table.selected_index()?];
        row.rep.map(|i| &self.table.procs[i])
    }

    /// Ends the selected process, or every process of the selected group.
    fn end_task(&mut self) {
        let Some(i) = self.table.selected_index() else { return };
        let row = &self.table.rows[i];
        let targets: Vec<(u32, i64)> = match row.key {
            Key::Proc(pid, created) => vec![(pid, created)],
            Key::Group(..) => self.table.keys(i..i + 1),
            Key::Section(_) => return,
        };
        if targets.iter().any(|&(pid, _)| suprvisor_core::is_critical(pid)) {
            let text = HSTRING::from(format!("{} is a critical Windows process. Ending it stops Windows immediately and unsaved data is lost.\n\nEnd it anyway?", row.label));
            let answer = unsafe { MessageBoxW(Some(self.hwnd), &text, w!("Suprvisor"), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) };
            if answer != IDYES {
                return;
            }
        }
        if targets.iter().map(|&(pid, created)| suprvisor_core::kill(pid, created)).any(|r| r.is_err()) {
            let _ = unsafe { windows::Win32::System::Diagnostics::Debug::MessageBeep(MB_ICONWARNING) };
        }
        let _ = self.signal.send(Signal::Rescan);
    }

    fn row_menu(&mut self) {
        let Some(i) = self.table.selected_index() else { return };
        let row = &self.table.rows[i];
        let end = match row.key {
            Key::Proc(..) => "End task".to_owned(),
            Key::Group(..) => format!("End all {} {}", row.procs.len(), row.label),
            Key::Section(_) => return,
        };
        let has_path = self.selected_proc().is_some_and(|p| !p.path.is_empty());
        let path_flags = if has_path { MF_STRING } else { MF_STRING | MF_GRAYED };
        let cmd = popup(self.hwnd, &[(MENU_END, MF_STRING, &end), (0, MF_SEPARATOR, ""), (MENU_OPEN, path_flags, "Open file location"), (MENU_COPY_PATH, path_flags, "Copy path"), (MENU_COPY_CMD, MF_STRING, "Copy command line")]);
        let Some(p) = self.selected_proc() else { return };
        let (pid, path) = (p.pid, p.path.clone());
        match cmd {
            MENU_END => self.end_task(),
            MENU_OPEN => shell::open_location(&path),
            MENU_COPY_PATH => shell::copy(self.hwnd, &path),
            MENU_COPY_CMD => shell::copy(self.hwnd, &suprvisor_core::command_line(pid).unwrap_or_default()),
            _ => {}
        }
    }

    fn header_menu(&mut self) {
        let flags = if self.table.grouped { MF_STRING | MF_CHECKED } else { MF_STRING };
        if popup(self.hwnd, &[(MENU_GROUPED, flags, "Group by type")]) == MENU_GROUPED {
            self.table.grouped = !self.table.grouped;
            self.rebuild();
            self.save();
        }
    }

    fn key(&mut self, vk: VIRTUAL_KEY) -> bool {
        if vk == VK_TAB && ctrl() {
            self.set_tab(!self.performance);
            return true;
        }
        if vk == VK_F && ctrl() {
            self.set_tab(false);
            self.set_focus(true);
            return true;
        }
        if self.performance {
            // The table is not on screen: no key may act on its selection (Delete ends a task).
            if vk == VK_ESCAPE {
                self.hide();
            }
            return vk == VK_ESCAPE;
        }
        let sel = self.table.selected_index();
        let last = self.table.rows.len().saturating_sub(1);
        let page = self.page();
        match vk {
            VK_ESCAPE if !self.table.filter.is_empty() || self.focus => {
                self.table.filter.clear();
                self.focus = false;
                self.rebuild();
            }
            VK_ESCAPE => self.hide(),
            VK_BACK => {
                self.table.filter.pop();
                self.rebuild();
            }
            VK_UP => self.select(sel.map_or(0, |i| i.saturating_sub(1))),
            VK_DOWN => self.select(sel.map_or(0, |i| (i + 1).min(last))),
            VK_PRIOR => self.select(sel.map_or(0, |i| i.saturating_sub(page))),
            VK_NEXT => self.select(sel.map_or(0, |i| (i + page).min(last))),
            VK_HOME => self.select(0),
            VK_END => self.select(last),
            VK_LEFT | VK_RIGHT | VK_RETURN => {
                let Some(i) = sel else { return true };
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
            VK_C if ctrl() => {
                if let Some(p) = self.selected_proc() {
                    let path = p.path.clone();
                    shell::copy(self.hwnd, &path);
                }
            }
            _ => return false,
        }
        true
    }
}

/// A native popup menu at the cursor. Returns the chosen id, 0 for none.
fn popup(hwnd: HWND, items: &[(usize, MENU_ITEM_FLAGS, &str)]) -> usize {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return 0 };
        for &(id, flags, text) in items {
            let _ = AppendMenuW(menu, flags, id, &HSTRING::from(text));
        }
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let _ = SetForegroundWindow(hwnd); // so the menu closes when clicking elsewhere
        let id = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_RETURNCMD, p.x, p.y, Some(0), hwnd, None).0 as usize;
        let _ = DestroyMenu(menu);
        id
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
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
            let frame = render::Frame {
                table: &a.table,
                system: &a.snap.system,
                icons: &a.snap.icons,
                widths: &a.settings.widths,
                processes: a.snap.procs.len(),
                performance: a.performance,
                history: &a.snap.history,
                nav_open: a.nav_open,
                focus: a.focus,
                hot: a.hot,
                can_end: a.can_end(),
                maximized: unsafe { IsZoomed(hwnd) }.as_bool(),
            };
            let _ = a.renderer.paint(hwnd, dpi(hwnd), &a.theme, &frame);
            let _ = unsafe { EndPaint(hwnd, &ps) };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1), // Direct2D paints everything
        WM_SIZE => {
            a.renderer.resize((lp.0 & 0xFFFF) as u32, ((lp.0 >> 16) & 0xFFFF) as u32);
            // WM_SIZE also comes at creation, while the resident window is still hidden.
            let visible = wp.0 as u32 != SIZE_MINIMIZED && unsafe { IsWindowVisible(hwnd) }.as_bool();
            let _ = a.signal.send(Signal::Visible(visible));
            a.changed();
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
        WM_APP_DATA => {
            a.load();
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = a.point(lp);
            match a.drag {
                Drag::Column(i, x0, w0) => {
                    a.settings.widths[i] = (w0 + x0 - x).clamp(32.0, 600.0);
                    invalidate(hwnd);
                }
                Drag::Thumb(grab) => {
                    let (_, h) = a.size();
                    let rows = a.table.rows.len();
                    if let Some((y0, y1)) = render::thumb(h, rows, 0) {
                        let span = h - TOP - HEAD - (y1 - y0);
                        let max = rows.saturating_sub(a.page());
                        a.table.scroll = ((y - grab - y0) / span * max as f32).round().clamp(0.0, max as f32) as usize;
                        a.changed();
                    }
                }
                Drag::Header(_) => {}
                Drag::None => {
                    let hit = a.hit(x, y);
                    a.set_hot(match hit {
                        Hit::Header(c) => Hot::Header(c),
                        Hit::Nav(k) => Hot::Nav(k),
                        Hit::Search => Hot::Search,
                        Hit::SearchClear => Hot::SearchClear,
                        Hit::EndTask => Hot::EndTask,
                        _ => Hot::None,
                    });
                    let hover = match hit {
                        Hit::Row(i, _) => Some(i),
                        _ => None,
                    };
                    if hover != a.table.hover {
                        a.table.hover = hover;
                        invalidate(hwnd);
                    }
                    let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    let _ = unsafe { TrackMouseEvent(&mut t) };
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            a.table.hover = None;
            a.hot = Hot::None;
            invalidate(hwnd);
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
            let _ = unsafe { windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut p) };
            let hit = a.hit(p.x as f32 * 96.0 / dpi(hwnd) as f32, p.y as f32 * 96.0 / dpi(hwnd) as f32);
            let cursor = match hit {
                _ if matches!(a.drag, Drag::Column(..)) => IDC_SIZEWE,
                Hit::Edge(_) => IDC_SIZEWE,
                Hit::Search => IDC_IBEAM,
                _ => IDC_ARROW,
            };
            let _ = unsafe { SetCursor(LoadCursorW(None, cursor).ok()) };
            LRESULT(1)
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let (x, y) = a.point(lp);
            let hit = a.hit(x, y);
            a.set_focus(matches!(hit, Hit::Search));
            match hit {
                Hit::Edge(i) => {
                    a.drag = Drag::Column(i, x, a.settings.widths[i]);
                    unsafe { SetCapture(hwnd) };
                }
                Hit::Header(c) => {
                    a.drag = Drag::Header(c);
                    unsafe { SetCapture(hwnd) };
                }
                Hit::Row(i, chevron) => {
                    a.select(i);
                    if chevron || msg == WM_LBUTTONDBLCLK {
                        a.table.toggle(i);
                        a.changed();
                    }
                }
                Hit::Thumb => {
                    if let Some((y0, _)) = render::thumb(a.size().1, a.table.rows.len(), a.table.scroll) {
                        a.drag = Drag::Thumb(y - y0);
                        unsafe { SetCapture(hwnd) };
                    }
                }
                Hit::Nav(0) => {
                    a.nav_open = !a.nav_open;
                    a.changed();
                    a.save();
                }
                Hit::Nav(k) => a.set_tab(k == 2),
                Hit::EndTask if a.can_end() => a.end_task(),
                Hit::SearchClear => {
                    a.table.filter.clear();
                    a.rebuild();
                }
                Hit::Track => {
                    let up = render::thumb(a.size().1, a.table.rows.len(), a.table.scroll).is_some_and(|(y0, _)| y < y0);
                    let page = a.page() as isize;
                    a.scroll_by(if up { -page } else { page });
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = a.point(lp);
            match a.drag {
                Drag::Column(..) => a.save(),
                Drag::Header(c) if matches!(a.hit(x, y), Hit::Header(h) if h == c) => {
                    a.table.sort_by(c);
                    a.changed();
                    let _ = unsafe { UpdateWindow(hwnd) }; // paint now; the settings file can wait
                    a.save();
                }
                _ => {}
            }
            if !matches!(a.drag, Drag::None) {
                a.drag = Drag::None;
                let _ = unsafe { ReleaseCapture() };
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = a.point(lp);
            match a.hit(x, y) {
                Hit::Row(i, _) => {
                    a.select(i);
                    a.row_menu();
                }
                Hit::Header(_) | Hit::Edge(_) => a.header_menu(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL if a.performance => LRESULT(0),
        WM_MOUSEWHEEL => {
            let delta = (wp.0 >> 16) as u16 as i16 as isize;
            a.scroll_by(-delta * 3 / 120);
            LRESULT(0)
        }
        WM_KEYDOWN if a.key(VIRTUAL_KEY(wp.0 as u16)) => LRESULT(0),
        WM_CHAR => {
            // Typing anywhere filters by name, window title or PID.
            if let Some(c) = char::from_u32(wp.0 as u32).filter(|c| !c.is_control() && !ctrl()) {
                a.table.filter.push(c);
                a.table.scroll = 0;
                a.focus = true;
                if a.performance {
                    a.set_tab(false); // rebuilds
                } else {
                    a.rebuild();
                }
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
        WM_CLOSE if !a.exiting => {
            a.hide(); // X hides; exit is in the tray menu
            LRESULT(0)
        }
        WM_APP_EXIT => {
            a.exiting = true;
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_APP_TRAY => {
            match (lp.0 & 0xFFFF) as u32 {
                WM_LBUTTONUP => {
                    if a.shown {
                        a.hide();
                    } else {
                        a.show();
                    }
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => match popup(hwnd, &[(MENU_SHOW, MF_STRING, "Show"), (0, MF_SEPARATOR, ""), (MENU_EXIT, MF_STRING, "Exit")]) {
                    MENU_SHOW => a.show(),
                    MENU_EXIT => {
                        a.exiting = true;
                        let _ = unsafe { DestroyWindow(hwnd) };
                    }
                    _ => {}
                },
                _ => {}
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            a.theme = theme::Theme::current();
            style_window(hwnd, a.theme.dark);
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            a.save();
            let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &tray_data(hwnd)) };
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(a as *mut App));
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        m if m == a.taskbar_created => {
            add_tray_icon(hwnd); // Explorer restarted
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
