//! Machine totals that are cheap to read directly.

use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessorPerformanceInformation};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{ALL_PROCESSOR_GROUPS, GetActiveProcessorCount, GetSystemTimes};

pub fn logical_cpus() -> u32 {
    unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }
}

/// (used, total) physical memory in bytes.
pub fn memory() -> (u64, u64) {
    let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    let _ = unsafe { GlobalMemoryStatusEx(&mut m) };
    (m.ullTotalPhys - m.ullAvailPhys, m.ullTotalPhys)
}

/// Dedicated VRAM of all hardware adapters, in bytes. Fixed while the machine runs.
pub fn vram_total() -> u64 {
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return 0 };
    (0..)
        .map_while(|i| unsafe { factory.EnumAdapters1(i) }.ok())
        .filter_map(|a| unsafe { a.GetDesc1() }.ok())
        .filter(|d| d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0)
        .map(|d| d.DedicatedVideoMemory as u64)
        .sum()
}

/// Whole-machine CPU use from `GetSystemTimes` deltas.
#[derive(Default)]
pub struct SystemTimes {
    prev: Option<(u64, u64)>, // (idle, kernel + user); kernel time includes idle time
    cores: Vec<(u64, u64)>,   // the same, per logical processor
}

/// SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CoreTimes {
    idle: i64,
    kernel: i64, // includes idle
    user: i64,
    dpc: i64,
    interrupt: i64,
    interrupts: u32,
}

/// Busy share 0-100 between two (idle, total) readings.
fn busy(prev: (u64, u64), now: (u64, u64)) -> f32 {
    if now.1 <= prev.1 {
        return 0.0;
    }
    ((1.0 - now.0.saturating_sub(prev.0) as f64 / (now.1 - prev.1) as f64) * 100.0).clamp(0.0, 100.0) as f32
}

impl SystemTimes {
    /// 0-100 since the previous call; 0 on the first call.
    pub fn cpu(&mut self) -> f32 {
        let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
        if unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.is_err() {
            return 0.0;
        }
        let ft = |f: FILETIME| (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64;
        let now = (ft(idle), ft(kernel) + ft(user));
        let pct = self.prev.map_or(0.0, |p| busy(p, now));
        self.prev = Some(now);
        pct
    }

    /// 0-100 per logical processor since the previous call; 0 on the first call. One kernel call (~5 us).
    /// ponytail: the processor group of this thread only (up to 64 logical processors); machines
    /// with more need NtQuerySystemInformationEx per group.
    pub fn cores(&mut self, n: u32) -> Vec<f32> {
        let mut buf = vec![CoreTimes::default(); n.max(1) as usize];
        let bytes = (buf.len() * size_of::<CoreTimes>()) as u32;
        let mut got = 0;
        let r = unsafe { NtQuerySystemInformation(SystemProcessorPerformanceInformation, buf.as_mut_ptr().cast(), bytes, &mut got) };
        if r.is_err() {
            return Vec::new();
        }
        buf.truncate(got as usize / size_of::<CoreTimes>());
        let now: Vec<(u64, u64)> = buf.iter().map(|c| (c.idle as u64, (c.kernel + c.user) as u64)).collect();
        let out = now.iter().enumerate().map(|(i, &t)| self.cores.get(i).map_or(0.0, |&p| busy(p, t))).collect();
        self.cores = now;
        out
    }
}
