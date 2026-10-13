//! Ctrl+Shift+Esc through a low-level keyboard hook. Explorer owns the hotkey (RegisterHotKey
//! fails with ERROR_HOTKEY_ALREADY_REGISTERED) and answers it by starting taskmgr.exe, which costs
//! a process start (~50 ms) before anything shows. The hook takes the keystroke first, so the
//! resident window shows with no process start. The IFEO redirect stays for the other ways
//! taskmgr starts (taskbar menu, Run box) and for when Suprvisor is not running.

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::*;

static TARGET: AtomicIsize = AtomicIsize::new(0);
static MESSAGE: AtomicIsize = AtomicIsize::new(0);

fn down(vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY) -> bool {
    (unsafe { GetAsyncKeyState(vk.0 as i32) }) < 0
}

unsafe extern "system" fn hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && (wp.0 as u32 == WM_KEYDOWN || wp.0 as u32 == WM_SYSKEYDOWN) {
        let k = unsafe { &*(lp.0 as *const KBDLLHOOKSTRUCT) };
        if k.vkCode == VK_ESCAPE.0 as u32 && down(VK_CONTROL) && down(VK_SHIFT) && !down(VK_MENU) && !down(VK_LWIN) && !down(VK_RWIN) {
            let hwnd = HWND(TARGET.load(Ordering::Relaxed) as *mut _);
            let _ = unsafe { PostMessageW(Some(hwnd), MESSAGE.load(Ordering::Relaxed) as u32, WPARAM(0), LPARAM(0)) };
            return LRESULT(1); // swallowed: Explorer never sees it
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

/// Posts `msg` to `hwnd` on each Ctrl+Shift+Esc. The hook runs on its own thread: Windows waits
/// for the hook on every keystroke system-wide, so it must never wait behind painting.
pub fn install(hwnd: HWND, msg: u32) {
    TARGET.store(hwnd.0 as isize, Ordering::Relaxed);
    MESSAGE.store(msg as isize, Ordering::Relaxed);
    std::thread::spawn(|| unsafe {
        if SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), None, 0).is_err() {
            return;
        }
        let mut m = MSG::default();
        while GetMessageW(&mut m, None, 0, 0).as_bool() {}
    });
}
