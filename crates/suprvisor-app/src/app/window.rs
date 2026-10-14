//! The window: its class, creation, the message loop, show, hide and close.

use std::sync::{Arc, Mutex, mpsc};

use windows::Win32::Graphics::Dwm::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_MOUSE, SendInput};
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

use super::messages::wndproc;
use super::*;
use crate::collector::{self, Snapshot};
use crate::{WM_APP_EXIT, WM_APP_HIDE, WM_APP_HOTKEY, WM_APP_SHOW, class, taskmgr, test};

/// Creates the window and runs the message loop until it closes. `resident`: start hidden.
pub fn run(resident: bool, activate: bool) -> Result<()> {
    // Like Task Manager: the window must answer when the machine is saturated.
    let _ = unsafe { SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS) };
    let instance = unsafe { GetModuleHandleW(None)? };
    register_class(instance)?;
    let (signal_tx, signal_rx) = mpsc::channel();
    let data = Arc::new(Mutex::new(Snapshot::default()));
    let wanted = Arc::new(Mutex::new(Vec::new()));
    let state = Box::new(App::new(data.clone(), wanted.clone(), signal_tx)?);
    let (window, performance) = (state.settings.window, state.performance);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP, // all content comes through DirectComposition (render/target.rs)
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
            Some(Box::into_raw(state).cast()),
        )?
    };
    place(hwnd, window);
    collector::spawn(hwnd, data, wanted, signal_rx, !performance, !resident);
    if !test() {
        // A test instance must not take the user's hotkey or add to the tray.
        taskmgr::install(hwnd, WM_APP_HOTKEY);
        super::tray::add(hwnd);
    }
    // Started elevated by the Start task; the IFEO stub and suprvisor.exe started from Explorer
    // are not. UIPI drops their messages unless allowed here.
    for m in [WM_APP_SHOW, WM_APP_HIDE, WM_APP_EXIT] {
        let _ = unsafe { ChangeWindowMessageFilterEx(hwnd, m, MSGFLT_ALLOW, None) };
    }
    if let Some(a) = app(hwnd) {
        a.nav_pill = a.pill_target();
        a.maximize_on_show = window.is_some_and(|(_, max)| max);
        if !resident {
            a.show_with(activate);
        }
    }
    message_loop(hwnd)
}

fn register_class(instance: HMODULE) -> Result<()> {
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        hIcon: app_icon(unsafe { GetSystemMetrics(SM_CXICON) }).unwrap_or_default(),
        hIconSm: app_icon(unsafe { GetSystemMetrics(SM_CXSMICON) }).unwrap_or_default(),
        lpszClassName: class(),
        ..Default::default()
    };
    unsafe { RegisterClassExW(&wc) };
    Ok(())
}

/// The app icon (res/app.rc) at `size` pixels.
pub(super) fn app_icon(size: i32) -> Option<HICON> {
    let instance = unsafe { GetModuleHandleW(None) }.ok()?;
    unsafe { LoadImageW(Some(instance.into()), PCWSTR(1 as _), IMAGE_ICON, size, size, LR_DEFAULTCOLOR) }.ok().map(|h| HICON(h.0))
}

/// Puts the hidden window at its saved place, and styles its frame.
fn place(hwnd: HWND, window: Option<([i32; 4], bool)>) {
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
    style_window(hwnd, Theme::current().dark);
    // WM_NCCALCSIZE at creation does not size the client area: ask again, so the title bar goes.
    let _ = unsafe { SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE) };
}

/// Input never waits for the display: all pending messages run first, then at most one frame,
/// drawn when the swapchain can take it (its waitable object). Many changes between two frames
/// cost one paint.
fn message_loop(hwnd: HWND) -> Result<()> {
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
        let Some(a) = app(hwnd) else { return Ok(()) };
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

pub(super) fn style_window(hwnd: HWND, dark: bool) {
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

/// Resize border height in pixels: the part of the title bar that resizes the window.
pub(super) fn frame(hwnd: HWND) -> i32 {
    let d = dpi(hwnd);
    unsafe { GetSystemMetricsForDpi(SM_CYFRAME, d) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, d) }
}

/// The caption button of a non-client hit code.
pub(super) fn caption(wp: WPARAM) -> Option<usize> {
    [HTMINBUTTON, HTMAXBUTTON, HTCLOSE].iter().position(|&h| h == wp.0 as u32)
}

impl App {
    pub(super) fn set_tab(&mut self, performance: bool) {
        if performance == self.performance {
            return;
        }
        self.performance = performance;
        self.drag = Drag::None;
        let _ = self.signal.send(Signal::Table(!performance));
        if performance && matches!(self.focus, Some(Focus::Header(_) | Focus::Rows | Focus::Command(1))) {
            self.focus = Some(Focus::Nav(2));
        }
        if !performance && self.focus == Some(Focus::GraphMode) {
            self.focus = Some(Focus::Nav(1));
        }
        if performance {
            self.dirty = true;
        } else {
            self.rebuild();
        }
        self.save_later();
    }

    pub(super) fn show(&mut self) {
        self.show_with(true);
    }

    pub(super) fn show_with(&mut self, activate: bool) {
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
                // last input. An empty mouse input (no move, no button) makes it the last input.
                // Joining the foreground thread's queue fails when no window has the foreground.
                let input = INPUT { r#type: INPUT_MOUSE, ..Default::default() };
                let _ = SendInput(&[input], size_of::<INPUT>() as i32);
                let _ = SetForegroundWindow(self.hwnd);
            }
        }
        if !self.shown {
            self.shown = true;
            let _ = self.signal.send(Signal::Visible(true));
        }
    }

    pub(super) fn hide(&mut self) {
        self.save();
        let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        self.shown = false;
        let _ = self.signal.send(Signal::Visible(false));
    }

    /// Ends the process (tray menu, `--exit`). Closing the window only hides it.
    pub(super) fn exit(&mut self) {
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}
