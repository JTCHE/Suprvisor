//! Started by Windows in place of taskmgr.exe. install.ps1 makes suprvisor.exe the IFEO "Debugger"
//! of taskmgr.exe, so every Task Manager start that the keyboard hook does not catch (taskbar menu,
//! Run box, Ctrl+Alt+Del screen) runs `suprvisor.exe --ifeo C:\...\taskmgr.exe [args]` instead.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

use windows::Win32::Foundation::{CloseHandle, LPARAM, WPARAM};
use windows::Win32::System::Diagnostics::Debug::{DebugActiveProcessStop, DebugSetProcessKillOnExit};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PWSTR, Result};

const RESIDENT_TASK: &str = r"\Suprvisor\Resident";
const TASKMGR_TASK: &str = r"\Suprvisor\Taskmgr";

/// Shows the resident window. No answer in 200 ms: starts the real Task Manager, and the
/// resident process again for the next time.
pub fn redirect(taskmgr: &[String]) -> Result<()> {
    if let Ok(h) = unsafe { FindWindowW(crate::class(), None) } {
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        let _ = unsafe { AllowSetForegroundWindow(pid) };
        let flags = SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT;
        if unsafe { SendMessageTimeoutW(h, crate::WM_APP_SHOW, WPARAM(0), LPARAM(0), flags, 200, None) }.0 != 0 {
            return Ok(());
        }
    }
    // The tasks run elevated without a UAC prompt (install.ps1). schtasks is a console program.
    let run = |task| {
        let mut c = Command::new("schtasks.exe");
        c.args(["/run", "/tn", task]).creation_flags(CREATE_NO_WINDOW.0).stdout(Stdio::null()).stderr(Stdio::null());
        c.status().is_ok_and(|s| s.success())
    };
    run(RESIDENT_TASK);
    if run(TASKMGR_TASK) {
        return Ok(());
    }
    real_taskmgr(taskmgr) // not installed: works when this process is elevated
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
