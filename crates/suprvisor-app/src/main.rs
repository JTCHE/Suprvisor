//! Suprvisor: a task manager. Ctrl+Shift+Esc shows it (keyboard hook, taskmgr/hotkey.rs, while it runs;
//! the IFEO redirect, taskmgr, while it does not), and so does running suprvisor.exe again.
//! The process stays resident in the tray: closing the window hides it, the tray menu exits.
//!   suprvisor.exe                 show (start it if none runs)
//!   suprvisor.exe --resident      start hidden (the Start task, at logon and from the IFEO stub)
//!   suprvisor.exe --inactive      start shown without taking the focus (audit/stress.ps1)
//!   suprvisor.exe --hide          hide
//!   suprvisor.exe --exit          quit the running process
//!   suprvisor.exe --ifeo ...      run by Windows in place of taskmgr.exe (taskmgr)
//!   suprvisor.exe --real-taskmgr  start the real Task Manager (the fallback task)
#![windows_subsystem = "windows"]

mod app;
mod collector;
mod edit;
mod render;
mod settings;
mod shell;
mod table;
mod taskmgr;
mod trace;

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

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
const WM_APP_TRAY: u32 = WM_APP + 9; // the tray icon's callback

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = args.first().map(String::as_str);
    match arg {
        Some("--ifeo") => return taskmgr::redirect(&args[1..]),
        Some("--real-taskmgr") => return taskmgr::real_taskmgr(&args[1..]),
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
    // Installed: run elevated through the Start task (no UAC prompt).
    if !resident && !test() && !taskmgr::elevated() && taskmgr::start_elevated() {
        return Ok(());
    }
    app::run(resident, arg != Some("--inactive"))
}
