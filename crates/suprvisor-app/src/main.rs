//! Suprvisor: a resident task manager. One process stays in the tray with its window hidden;
//! running `suprvisor.exe` again only signals it to show (later, the Ctrl+Shift+Esc IFEO path).
//!   suprvisor.exe          show (start the resident process if none runs)
//!   suprvisor.exe --hide   hide
//!   suprvisor.exe --exit   quit the resident process
#![windows_subsystem = "windows"]

mod render;
mod theme;

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use suprvisor_core::{Collector, System};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

const CLASS: PCWSTR = w!("Suprvisor");
const WM_APP_SHOW: u32 = WM_APP + 1;
const WM_APP_HIDE: u32 = WM_APP + 2;
const WM_APP_EXIT: u32 = WM_APP + 3;
const WM_APP_DATA: u32 = WM_APP + 4; // the collector thread has a new snapshot
const WM_APP_TRAY: u32 = WM_APP + 5;
const MENU_SHOW: usize = 1;
const MENU_EXIT: usize = 2;

/// What the collector thread hands to the UI thread.
#[derive(Default)]
struct Snapshot {
    system: System,
    processes: usize,
}

struct App {
    hwnd: HWND,
    renderer: render::Renderer,
    theme: theme::Theme,
    data: Arc<Mutex<Snapshot>>,
    visible: mpsc::Sender<bool>,
    taskbar_created: u32,
    exiting: bool,
}

fn main() -> Result<()> {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)? };
    let arg = std::env::args().nth(1);
    let msg = match arg.as_deref() {
        Some("--hide") => WM_APP_HIDE,
        Some("--exit") => WM_APP_EXIT,
        _ => WM_APP_SHOW,
    };
    // A resident instance exists: signal it and leave. This process got the user's input, so it may
    // pass the right to take the foreground to the resident one.
    if let Ok(existing) = unsafe { FindWindowW(CLASS, None) } {
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(existing, Some(&mut pid)) };
        let _ = unsafe { AllowSetForegroundWindow(pid) };
        unsafe { PostMessageW(Some(existing), msg, WPARAM(0), LPARAM(0))? };
        return Ok(());
    }
    if msg != WM_APP_SHOW {
        return Ok(()); // --hide / --exit with nothing running
    }

    let instance = unsafe { GetModuleHandleW(None)? };
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION)? },
        lpszClassName: CLASS,
        ..Default::default()
    };
    unsafe { RegisterClassExW(&wc) };

    let (visible_tx, visible_rx) = mpsc::channel();
    let data = Arc::new(Mutex::new(Snapshot::default()));
    let app = Box::new(App {
        hwnd: HWND::default(),
        renderer: render::Renderer::new()?,
        theme: theme::Theme::current(),
        data: data.clone(),
        visible: visible_tx,
        taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
        exiting: false,
    });
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
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
    style_window(hwnd, theme::Theme::current().dark);
    add_tray_icon(hwnd);
    spawn_collector(hwnd, data, visible_rx);
    show(hwnd);

    let mut m = MSG::default();
    while unsafe { GetMessageW(&mut m, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
    }
    Ok(())
}

/// Collector thread: samples while the window is visible, sleeps while it is hidden.
fn spawn_collector(hwnd: HWND, data: Arc<Mutex<Snapshot>>, visible: mpsc::Receiver<bool>) {
    let hwnd = hwnd.0 as usize; // HWND is not Send; the value is valid on any thread
    std::thread::spawn(move || {
        let Ok(mut c) = Collector::new() else { return };
        let mut processes = c.scan().len();
        let (mut shown, mut tick) = (false, 0u64);
        loop {
            let wait = if shown { visible.recv_timeout(Duration::from_millis(500)) } else { visible.recv().map_err(|_| RecvTimeoutError::Disconnected) };
            match wait {
                Ok(v) => shown = v,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if !shown {
                continue;
            }
            // Fast tick 500 ms; process list every 3 s; kernel list every 60 s (see suprvisor-core).
            if tick % 6 == 0 {
                processes = c.scan().len();
            }
            if tick % 120 == 0 {
                let _ = c.scan_full();
            }
            tick += 1;
            *data.lock().unwrap() = Snapshot { system: c.system(), processes };
            let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_DATA, WPARAM(0), LPARAM(0)) };
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

fn show(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn app<'a>(hwnd: HWND) -> Option<&'a mut App> {
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut App;
    unsafe { p.as_mut() }
}

fn dpi(hwnd: HWND) -> u32 {
    unsafe { GetDpiForWindow(hwnd) }
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
            let d = a.data.lock().unwrap();
            let _ = a.renderer.paint(hwnd, dpi(hwnd), &a.theme, &d.system, d.processes);
            drop(d);
            let _ = unsafe { EndPaint(hwnd, &ps) };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1), // Direct2D paints everything
        WM_SIZE => {
            a.renderer.resize((lp.0 & 0xFFFF) as u32, ((lp.0 >> 16) & 0xFFFF) as u32);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let r = unsafe { &*(lp.0 as *const RECT) };
            a.renderer.set_dpi((wp.0 & 0xFFFF) as u32);
            let _ = unsafe { SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE) };
            LRESULT(0)
        }
        WM_SHOWWINDOW => {
            let _ = a.visible.send(wp.0 != 0);
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_APP_DATA => {
            let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            LRESULT(0)
        }
        WM_APP_SHOW => {
            show(hwnd);
            LRESULT(0)
        }
        WM_APP_HIDE => {
            let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
            LRESULT(0)
        }
        WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
            let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
            LRESULT(0)
        }
        WM_CLOSE if !a.exiting => {
            let _ = unsafe { ShowWindow(hwnd, SW_HIDE) }; // X hides; exit is in the tray menu
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
                    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
                        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
                    } else {
                        show(hwnd);
                    }
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => tray_menu(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            match wp.0 & 0xFFFF {
                MENU_SHOW => show(hwnd),
                MENU_EXIT => {
                    a.exiting = true;
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            a.theme = theme::Theme::current();
            style_window(hwnd, a.theme.dark);
            let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            LRESULT(0)
        }
        WM_DESTROY => {
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

fn tray_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, MENU_SHOW, w!("Show"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Exit"));
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let _ = SetForegroundWindow(hwnd); // so the menu closes when clicking elsewhere
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, p.x, p.y, Some(0), hwnd, None);
        let _ = DestroyMenu(menu);
    }
}
