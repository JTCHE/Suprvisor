//! The tray icon: the process stays resident with its window hidden. A click shows or hides the
//! window; the menu has Show and Exit.

use std::sync::OnceLock;

use windows::Win32::Foundation::*;
use windows::Win32::UI::Shell::{NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

use super::App;
use super::window::app_icon;
use crate::WM_APP_TRAY;

const MENU_SHOW: usize = 1;
const MENU_EXIT: usize = 2;

/// Explorer sends this message to all top-level windows when it restarts: the icon is gone.
fn taskbar_created() -> u32 {
    static ID: OnceLock<u32> = OnceLock::new();
    *ID.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut d = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_APP_TRAY,
        hIcon: app_icon(unsafe { GetSystemMetrics(SM_CXSMICON) }).unwrap_or_default(),
        ..Default::default()
    };
    for (i, c) in "Suprvisor".encode_utf16().enumerate() {
        d.szTip[i] = c;
    }
    d
}

pub(super) fn add(hwnd: HWND) {
    let _ = unsafe { Shell_NotifyIconW(NIM_ADD, &data(hwnd)) };
}

pub(super) fn remove(hwnd: HWND) {
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data(hwnd)) };
}

/// The tray icon's messages.
pub(super) fn tray_message(a: &mut App, hwnd: HWND, msg: u32, _wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if msg == taskbar_created() {
        add(hwnd);
        return Some(LRESULT(0));
    }
    if msg != WM_APP_TRAY {
        return None;
    }
    match (lp.0 & 0xFFFF) as u32 {
        WM_LBUTTONUP if a.shown => a.hide(),
        WM_LBUTTONUP => a.show(),
        WM_RBUTTONUP | WM_CONTEXTMENU => match a.popup(&[(MENU_SHOW, MF_STRING, "Show"), (0, MF_SEPARATOR, ""), (MENU_EXIT, MF_STRING, "Exit")], None) {
            MENU_SHOW => a.show(),
            MENU_EXIT => a.exit(),
            _ => {}
        },
        _ => {}
    }
    Some(LRESULT(0))
}
