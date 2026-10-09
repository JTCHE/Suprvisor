//! Machine totals that are cheap to read directly.

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
        let pct = match self.prev {
            Some((pi, pt)) if now.1 > pt => {
                let total = (now.1 - pt) as f64;
                ((1.0 - (now.0 - pi) as f64 / total) * 100.0).clamp(0.0, 100.0) as f32
            }
            _ => 0.0,
        };
        self.prev = Some(now);
        pct
    }
}
