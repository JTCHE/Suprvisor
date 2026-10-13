//! Started by Windows in place of taskmgr.exe. install.ps1 makes suprvisor.exe the IFEO "Debugger"
//! of taskmgr.exe, so every Task Manager start that the keyboard hook does not catch (taskbar menu,
//! Run box, Ctrl+Alt+Del screen) runs `suprvisor.exe --ifeo C:\...\taskmgr.exe [args]` instead.

mod hotkey;

pub use hotkey::install;

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::System::Diagnostics::Debug::{DebugActiveProcessStop, DebugSetProcessKillOnExit};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PWSTR, Result};

const START_TASK: &str = r"\Suprvisor\Start";
const TASKMGR_TASK: &str = r"\Suprvisor\Taskmgr";

/// Shows Suprvisor: the running one, or a new one through the installed task (elevated, no UAC
/// prompt). No answer in 200 ms from a running one, or not installed: the real Task Manager.
pub fn redirect(taskmgr: &[String]) -> Result<()> {
    if let Ok(h) = unsafe { FindWindowW(crate::class(), None) } {
        if show(h) {
            return Ok(());
        }
    } else if start_elevated() {
        return Ok(());
    }
    if run(TASKMGR_TASK) {
        return Ok(());
    }
    real_taskmgr(taskmgr) // not installed: works when this process is elevated
}

/// Asks the window `h` to show, and passes it the right to take the foreground.
fn show(h: HWND) -> bool {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    let _ = unsafe { AllowSetForegroundWindow(pid) };
    let flags = SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT;
    unsafe { SendMessageTimeoutW(h, crate::WM_APP_SHOW, WPARAM(0), LPARAM(0), flags, 200, None) }.0 != 0
}

/// Runs an installed task (install.ps1). The tasks run elevated without a UAC prompt.
fn run(task: &str) -> bool {
    let mut c = Command::new("schtasks.exe"); // a console program
    c.args(["/run", "/tn", task]).creation_flags(CREATE_NO_WINDOW.0).stdout(Stdio::null()).stderr(Stdio::null());
    c.status().is_ok_and(|s| s.success())
}

/// Starts Suprvisor elevated through the Start task, then shows it. False when not installed.
/// The task starts it hidden: only this process holds the right to take the foreground, so the
/// window shows when this process asks.
pub fn start_elevated() -> bool {
    if !run(START_TASK) {
        return false;
    }
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < until {
        if let Ok(h) = unsafe { FindWindowW(crate::class(), None) } {
            return show(h);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

/// True when this process runs elevated.
pub fn elevated() -> bool {
    use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    let mut token = Default::default();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let mut e = TOKEN_ELEVATION::default();
    let mut n = 0;
    let ok = unsafe { GetTokenInformation(token, TokenElevation, Some((&raw mut e).cast()), size_of::<TOKEN_ELEVATION>() as u32, &mut n) }.is_ok();
    let _ = unsafe { CloseHandle(token) };
    ok && e.TokenIsElevated != 0
}

/// Opens the Windows Task Manager, past the IFEO redirect: directly when this process is elevated,
/// else through the installed task, else (not installed, so no redirect) a plain start.
pub fn open_native() {
    if real_taskmgr(&[]).is_ok() || run(TASKMGR_TASK) {
        return;
    }
    let _ = Command::new("taskmgr.exe").spawn();
}

/// Starts Task Manager (or `cmd`) past the IFEO redirect: Windows skips the "Debugger" lookup for
/// a process created for debugging. The debugger detaches at once.
pub fn real_taskmgr(cmd: &[String]) -> Result<()> {
    let line = if cmd.is_empty() {
        format!(r"{}\System32\Taskmgr.exe", std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()))
    } else {
        cmd.iter().map(|a| if a.contains(' ') || a.is_empty() { format!("\"{a}\"") } else { a.clone() }).collect::<Vec<_>>().join(" ")
    };
    let mut w: Vec<u16> = line.encode_utf16().chain([0]).collect();
    let si = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(None, Some(PWSTR(w.as_mut_ptr())), None, None, false, DEBUG_ONLY_THIS_PROCESS, None, None, &si, &mut pi)?;
        let _ = DebugSetProcessKillOnExit(false);
        let _ = DebugActiveProcessStop(pi.dwProcessId);
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
    }
    Ok(())
}
