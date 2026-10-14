//! The window procedure.

use std::time::Instant;

use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::Threading::GetCurrentThread;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::nonclient::{caption_message, frame_message};
use super::*;
use super::keys::{TEST_MODS, ctrl, down, shift};
use super::window::style_window;
use crate::{WM_APP_DATA, WM_APP_EXIT, WM_APP_HIDE, WM_APP_HOTKEY, WM_APP_ICONS, WM_APP_SHOW, WM_APP_TEST_MODS, WM_APP_TEST_SNAP, test};

pub(super) extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
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
    for group in [window_message, life_message, super::tray::tray_message, app_message, frame_message, caption_message, mouse_message, key_message] {
        if let Some(r) = group(a, hwnd, msg, wp, lp) {
            return r; // after WM_DESTROY, `a` is gone: return at once
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// Painting, sizing, timers.
fn window_message(a: &mut App, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    Some(match msg {
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
            on_timer(a, hwnd, wp);
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
        _ => return None,
    })
}

fn on_timer(a: &mut App, hwnd: HWND, wp: WPARAM) {
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
        TIMER_TOOLTIP => {
            let _ = unsafe { KillTimer(Some(hwnd), TIMER_TOOLTIP) };
            if let Hot::Nav(k) = a.hot
                && !a.nav_open
            {
                a.tooltip = Some(k);
                a.dirty = true;
            }
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
}

/// Show, activation, theme, close and destroy.
fn life_message(a: &mut App, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    Some(match msg {
        WM_SHOWWINDOW => {
            let _ = a.signal.send(Signal::Visible(wp.0 != 0));
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_ACTIVATE => {
            a.active = (wp.0 & 0xFFFF) as u32 != WA_INACTIVE;
            a.dirty = true;
            LRESULT(0)
        }
        WM_CLOSE => {
            a.hide(); // the process stays in the tray
            LRESULT(0)
        }
        WM_APP_EXIT => {
            a.exit();
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            a.theme = Theme::current();
            style_window(hwnd, a.theme.dark);
            a.dirty = true;
            LRESULT(0)
        }
        WM_DESTROY => {
            a.save();
            super::tray::remove(hwnd);
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(a as *mut App));
                PostQuitMessage(0);
                trace::flush();
            }
            LRESULT(0)
        }
        _ => return None,
    })
}

/// Our own messages: data, icons, show and hide, the test harness.
fn app_message(a: &mut App, hwnd: HWND, msg: u32, wp: WPARAM, _lp: LPARAM) -> Option<LRESULT> {
    Some(match msg {
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
        _ => return None,
    })
}

/// Mouse input on the client area.
fn mouse_message(a: &mut App, _hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    Some(match msg {
        WM_MOUSEMOVE => {
            let (x, y) = a.point(lp);
            a.mouse_move(x, y);
            LRESULT(0)
        }
        // Test instances get posted moves while the real cursor is elsewhere: no leave.
        WM_MOUSELEAVE if !test() => {
            if matches!(a.drag, Drag::Header(..) | Drag::Row(..)) {
                a.drag = Drag::None; // pressed without capture: the release will not come here
            }
            a.mouse = None;
            a.table.hover = None;
            a.set_hot(Hot::None);
            a.dirty = true;
            LRESULT(0)
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
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL if a.performance => LRESULT(0),
        // Tilt wheel and touchpad sideways, or Shift with the wheel: the columns scroll.
        WM_MOUSEHWHEEL => {
            a.hscroll_by((wp.0 >> 16) as u16 as i16 as f32 / 120.0 * 48.0);
            LRESULT(0)
        }
        WM_MOUSEWHEEL if shift() => {
            a.hscroll_by(-((wp.0 >> 16) as u16 as i16 as f32) / 120.0 * 48.0);
            LRESULT(0)
        }
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
        _ => return None,
    })
}

/// Keyboard input and the keyboard context menu.
fn key_message(a: &mut App, _hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    Some(match msg {
        // Shift+F10 and the menu key: the focused element's menu. lParam -1 marks the keyboard.
        WM_CONTEXTMENU if lp.0 == -1 => {
            a.focus_visible = true;
            a.context_menu(None);
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
        _ => return None,
    })
}
