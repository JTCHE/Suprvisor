//! All processes in one `NtQuerySystemInformation(SystemProcessInformation)` call.

use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation};
use windows::Win32::Foundation::{STATUS_BUFFER_TOO_SMALL, STATUS_INFO_LENGTH_MISMATCH};

/// Full x64 layout (the documented struct hides most fields). Same layout as phnt / System Informer.
#[repr(C)]
struct SystemProcessInfo {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    number_of_threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name_len: u16, // UNICODE_STRING, in bytes
    image_name_max: u16,
    image_name_buf: *const u16,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    handle_count: u32,
    session_id: u32,
    unique_process_key: usize,
    peak_virtual_size: usize,
    virtual_size: usize,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_page_count: usize,
    read_operation_count: i64,
    write_operation_count: i64,
    other_operation_count: i64,
    read_transfer_count: i64,
    write_transfer_count: i64,
    other_transfer_count: i64,
    // SYSTEM_THREAD_INFORMATION[number_of_threads] follows.
}
const _: () = assert!(std::mem::offset_of!(SystemProcessInfo, unique_process_id) == 80);
const _: () = assert!(std::mem::size_of::<SystemProcessInfo>() == 256);

/// Monotonic counters that rates are computed from.
#[derive(Clone, Copy, Debug)]
pub struct Counters {
    /// Kernel + user time, 100 ns units.
    pub cpu_time: i64,
    pub read: u64,
    pub write: u64,
}

pub struct Raw {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: String,
    pub create_time: i64,
    pub private_ws: u64,
    pub counters: Counters,
}

pub struct Procs {
    buf: Vec<u64>, // u64 keeps the 8-byte alignment the entries need
}

impl Procs {
    pub fn new() -> Self {
        Self { buf: vec![0; 256 * 1024] } // 2 MB, grows if needed
    }

    pub fn query(&mut self) -> windows::core::Result<Vec<Raw>> {
        loop {
            let mut needed = 0u32;
            let bytes = (self.buf.len() * 8) as u32;
            let status = unsafe {
                NtQuerySystemInformation(SystemProcessInformation, self.buf.as_mut_ptr().cast(), bytes, &mut needed)
            };
            if status == STATUS_INFO_LENGTH_MISMATCH || status == STATUS_BUFFER_TOO_SMALL {
                // Processes can start between the two calls: add headroom.
                self.buf.resize((needed as usize + 64 * 1024) / 8, 0);
                continue;
            }
            status.ok()?;
            break;
        }

        let mut out = Vec::with_capacity(1024);
        let base = self.buf.as_ptr().cast::<u8>();
        let mut offset = 0usize;
        loop {
            // SAFETY: the kernel wrote a chain of entries into buf; offsets stay inside it.
            let e = unsafe { &*base.add(offset).cast::<SystemProcessInfo>() };
            let pid = e.unique_process_id as u32;
            if pid != 0 {
                // pid 0 is the Idle process: Task Manager does not list it.
                let name = if e.image_name_buf.is_null() {
                    String::from("System")
                } else {
                    let s = unsafe { std::slice::from_raw_parts(e.image_name_buf, e.image_name_len as usize / 2) };
                    String::from_utf16_lossy(s)
                };
                out.push(Raw {
                    pid,
                    parent_pid: e.inherited_from_unique_process_id as u32,
                    name,
                    create_time: e.create_time,
                    private_ws: e.working_set_private_size as u64,
                    counters: Counters {
                        cpu_time: e.kernel_time + e.user_time,
                        read: e.read_transfer_count as u64,
                        write: e.write_transfer_count as u64,
                    },
                });
            }
            if e.next_entry_offset == 0 {
                break;
            }
            offset += e.next_entry_offset as usize;
        }
        Ok(out)
    }
}

