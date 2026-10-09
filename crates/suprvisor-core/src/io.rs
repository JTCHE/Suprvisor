//! System disk and network throughput from the drivers' own counters. PDH gave the same numbers
//! for ~4 ms per read, and the full interface table (GetIfTable2, 61 rows here) costs 2-3 ms.
//! Reading only the physical adapters costs ~0.1 ms each, so the hidden 1 s history sample fits
//! its budget.

use std::time::Instant;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfEntry2, GetIfTable2Ex, MIB_IF_ROW2, MIB_IF_TABLE2, MibIfTableRaw};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{DISK_PERFORMANCE, IOCTL_DISK_PERFORMANCE};
use windows::core::HSTRING;

#[derive(Default)]
pub struct Sample {
    pub disk_read: u64,
    pub disk_write: u64,
    pub net_recv: u64,
    pub net_sent: u64,
}

pub struct Io {
    disks: Vec<HANDLE>,
    adapters: Vec<u32>, // interface indexes
    calls: u64,
    prev: Option<(Instant, [u64; 4])>,
}

// Plain kernel handles, usable from any thread.
unsafe impl Send for Io {}

impl Io {
    /// ponytail: disks are found once; a drive plugged in later is not counted until restart.
    /// Opening a physical drive needs admin; without it, disk reads 0.
    pub fn new() -> Self {
        let disks = (0..32)
            .filter_map(|n| {
                let path = HSTRING::from(format!(r"\.\PhysicalDrive{n}"));
                // No access rights: IOCTL_DISK_PERFORMANCE is FILE_ANY_ACCESS.
                unsafe { CreateFileW(&path, 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None) }.ok()
            })
            .collect();
        Self { disks, adapters: Vec::new(), calls: 0, prev: None }
    }

    /// Bytes per second since the previous call; zeros on the first call.
    pub fn collect(&mut self) -> Sample {
        let now = Instant::now();
        let mut total = [0u64; 4];
        for &d in &self.disks {
            let mut p = DISK_PERFORMANCE::default();
            let mut n = 0;
            let out = Some((&raw mut p).cast());
            if unsafe { DeviceIoControl(d, IOCTL_DISK_PERFORMANCE, None, 0, out, size_of::<DISK_PERFORMANCE>() as u32, Some(&mut n), None) }.is_ok() {
                total[0] += p.BytesRead as u64;
                total[1] += p.BytesWritten as u64;
            }
        }
        // Adapters come and go (VPN, USB, Wi-Fi): list them again every 60 calls.
        if self.calls.is_multiple_of(60) {
            self.adapters = adapters();
            self.prev = None; // the sum covers other adapters now
        }
        self.calls += 1;
        for &i in &self.adapters {
            let mut r = MIB_IF_ROW2 { InterfaceIndex: i, ..Default::default() };
            if unsafe { GetIfEntry2(&mut r) }.is_ok() {
                total[2] += r.InOctets;
                total[3] += r.OutOctets;
            }
        }
        let sample = match self.prev {
            Some((at, prev)) => {
                let secs = (now - at).as_secs_f64().max(1e-3);
                let rate = |i: usize| (total[i].saturating_sub(prev[i]) as f64 / secs) as u64;
                Sample { disk_read: rate(0), disk_write: rate(1), net_recv: rate(2), net_sent: rate(3) }
            }
            None => Sample::default(),
        };
        self.prev = Some((now, total));
        sample
    }
}

impl Drop for Io {
    fn drop(&mut self) {
        for &d in &self.disks {
            let _ = unsafe { CloseHandle(d) };
        }
    }
}

/// Physical adapters. Filter and virtual interfaces carry the same traffic again.
fn adapters() -> Vec<u32> {
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    if unsafe { GetIfTable2Ex(MibIfTableRaw, &mut table) }.is_err() {
        return Vec::new();
    }
    let rows = unsafe { std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize) };
    // Bit 0: HardwareInterface, bit 1: FilterInterface.
    let list = rows.iter().filter(|r| r.InterfaceAndOperStatusFlags._bitfield & 3 == 1).map(|r| r.InterfaceIndex).collect();
    unsafe { FreeMibTable(table.cast()) };
    list
}
