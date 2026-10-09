//! Per-process reads through handles. The cost grows with the processes read, not with the
//! thread count of the whole machine, which makes `NtQuerySystemInformation` slow (see lib.rs).

use windows::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, NTSTATUS, STILL_ACTIVE};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX2};
use windows::Win32::System::Threading::*;
use windows::core::PWSTR;

#[link(name = "ntdll")]
unsafe extern "system" {
    // Undocumented but stable since Vista; System Informer uses it. Opens the next process
    // after `process` (or the first one, for a null handle) with `access`.
    fn NtGetNextProcess(process: HANDLE, access: u32, attributes: u32, flags: u32, next: *mut HANDLE) -> NTSTATUS;
}

/// Handles to all running processes that `PROCESS_QUERY_LIMITED_INFORMATION` can open.
/// The caller owns and must close them.
pub fn walk() -> Vec<HANDLE> {
    let mut out = Vec::with_capacity(1024);
    let mut h = HANDLE::default();
    loop {
        let mut next = HANDLE::default();
        let status = unsafe { NtGetNextProcess(h, PROCESS_QUERY_LIMITED_INFORMATION.0, 0, 0, &mut next) };
        if !h.is_invalid() {
            // Exited processes stay listed while someone holds a handle: skip them.
            let mut code = 0;
            if unsafe { GetExitCodeProcess(h, &mut code) }.is_ok() && code == STILL_ACTIVE.0 as u32 {
                out.push(h);
            } else {
                close(h);
            }
        }
        if status.is_err() {
            return out;
        }
        h = next;
    }
}

pub fn close(h: HANDLE) {
    let _ = unsafe { CloseHandle(h) };
}

pub fn pid(h: HANDLE) -> u32 {
    unsafe { GetProcessId(h) }
}

fn ft(f: FILETIME) -> i64 {
    ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as i64
}

/// (create time, kernel + user time in 100 ns units).
pub fn times(h: HANDLE) -> Option<(i64, i64)> {
    let (mut c, mut e, mut k, mut u) = Default::default();
    unsafe { GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) }.ok()?;
    Some((ft(c), ft(k) + ft(u)))
}

pub fn private_ws(h: HANDLE) -> Option<u64> {
    let mut m = PROCESS_MEMORY_COUNTERS_EX2::default();
    let size = size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
    unsafe { GetProcessMemoryInfo(h, (&raw mut m).cast(), size) }.ok()?;
    Some(m.PrivateWorkingSetSize as u64)
}

/// (read, write) bytes since the process started. Slow (~150k cycles): call it rarely.
pub fn io(h: HANDLE) -> Option<(u64, u64)> {
    let mut c = IO_COUNTERS::default();
    unsafe { GetProcessIoCounters(h, &mut c) }.ok()?;
    Some((c.ReadTransferCount, c.WriteTransferCount))
}

/// Exe file name, for example `houdini.exe`. None for minimal processes (Registry, Memory Compression).
pub fn name(h: HANDLE) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    Some(path.rsplit('\\').next().unwrap_or(&path).to_owned())
}

pub fn parent_pid(h: HANDLE) -> u32 {
    let mut b = PROCESS_BASIC_INFORMATION::default();
    let size = size_of::<PROCESS_BASIC_INFORMATION>() as u32;
    let status = unsafe { NtQueryInformationProcess(h, ProcessBasicInformation, (&raw mut b).cast(), size, &mut 0) };
    if status.is_ok() { b.InheritedFromUniqueProcessId as u32 } else { 0 }
}

/// A handle that D3DKMT accepts (`PROCESS_QUERY_INFORMATION`), if allowed.
pub fn open_full(pid: u32) -> Option<HANDLE> {
    unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, false, pid) }.ok()
}

pub fn open_limited(pid: u32) -> Option<HANDLE> {
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()
}

