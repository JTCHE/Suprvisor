//! The non-client area: the title bar is client area, so Windows still moves, snaps and resizes
//! the window from the hit codes here.

use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::*;
use super::view::Hit;
use super::window::{caption, frame};

/// The custom title bar: client area, hit test, cursor.
pub(super) fn frame_message(a: &mut App, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    Some(match msg {
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
                return Some(d);
            }
            let mut p = POINT { x: lp.0 as i16 as i32, y: (lp.0 >> 16) as i16 as i32 };
            let _ = unsafe { ScreenToClient(hwnd, &mut p) };
            let maximized = unsafe { IsZoomed(hwnd) }.as_bool();
            if !maximized && p.y < frame(hwnd) {
                return Some(LRESULT(HTTOP as isize));
            }
            let s = 96.0 / dpi(hwnd) as f32;
            LRESULT(match a.hit(p.x as f32 * s, p.y as f32 * s) {
                Hit::Caption(k) => [HTMINBUTTON, HTMAXBUTTON, HTCLOSE][k],
                Hit::Title => HTCAPTION,
                _ => HTCLIENT,
            } as isize)
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
        _ => return None,
    })
}

/// Caption buttons: hover, and act on release over the same button.
pub(super) fn caption_message(a: &mut App, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    Some(match msg {
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
        _ => return None,
    })
}
