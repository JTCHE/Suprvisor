//! Performance counters (PDH): system disk and network throughput. Few instances, so cheap.
//! GPU data comes from `gpu.rs` instead: the PDH GPU sets are too large to collect every tick.

use windows::Win32::System::Performance::*;
use windows::core::{HSTRING, PCWSTR};

#[derive(Default)]
pub struct Sample {
    pub disk_read: u64,
    pub disk_write: u64,
    pub net_recv: u64,
    pub net_sent: u64,
}

pub struct Pdh {
    query: PDH_HQUERY,
    disk_read: PDH_HCOUNTER,
    disk_write: PDH_HCOUNTER,
    net_recv: PDH_HCOUNTER,
    net_sent: PDH_HCOUNTER,
    buf: Vec<u64>, // reused item buffer, u64 for alignment
}

// PDH handles may be used from any thread.
unsafe impl Send for Pdh {}

fn check(status: u32) -> windows::core::Result<()> {
    if status == 0 { Ok(()) } else { Err(windows::core::HRESULT(status as i32).into()) }
}

impl Pdh {
    pub fn new() -> windows::core::Result<Self> {
        let mut query = PDH_HQUERY::default();
        check(unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) })?;
        let add = |path: &str| -> windows::core::Result<PDH_HCOUNTER> {
            let mut c = PDH_HCOUNTER::default();
            check(unsafe { PdhAddEnglishCounterW(query, &HSTRING::from(path), 0, &mut c) })?;
            Ok(c)
        };
        let pdh = Self {
            query,
            disk_read: add(r"\PhysicalDisk(_Total)\Disk Read Bytes/sec")?,
            disk_write: add(r"\PhysicalDisk(_Total)\Disk Write Bytes/sec")?,
            net_recv: add(r"\Network Interface(*)\Bytes Received/sec")?,
            net_sent: add(r"\Network Interface(*)\Bytes Sent/sec")?,
            buf: vec![0; 4 * 1024],
        };
        // Rate counters need two collects; prime the first one now.
        check(unsafe { PdhCollectQueryData(query) })?;
        Ok(pdh)
    }

    pub fn collect(&mut self) -> windows::core::Result<Sample> {
        check(unsafe { PdhCollectQueryData(self.query) })?;
        Ok(Sample {
            disk_read: self.single(self.disk_read),
            disk_write: self.single(self.disk_write),
            net_recv: self.sum(self.net_recv),
            net_sent: self.sum(self.net_sent),
        })
    }

    fn single(&self, c: PDH_HCOUNTER) -> u64 {
        let mut v = PDH_FMT_COUNTERVALUE::default();
        if unsafe { PdhGetFormattedCounterValue(c, PDH_FMT_LARGE, None, &mut v) } != 0 || v.CStatus != 0 {
            return 0;
        }
        unsafe { v.Anonymous.largeValue as u64 }
    }

    /// Sum over all instances with valid data.
    fn sum(&mut self, c: PDH_HCOUNTER) -> u64 {
        loop {
            let (mut bytes, mut count) = ((self.buf.len() * 8) as u32, 0u32);
            let status = unsafe {
                PdhGetFormattedCounterArrayW(c, PDH_FMT_LARGE, &mut bytes, &mut count, Some(self.buf.as_mut_ptr().cast()))
            };
            if status == PDH_MORE_DATA {
                self.buf.resize(bytes as usize / 8 + 256, 0);
                continue;
            }
            if status != 0 {
                return 0; // no instances: treat as no data
            }
            let items = unsafe {
                std::slice::from_raw_parts(self.buf.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(), count as usize)
            };
            return items.iter().filter(|it| it.FmtValue.CStatus == 0).map(|it| unsafe { it.FmtValue.Anonymous.largeValue } as u64).sum();
        }
    }
}

impl Drop for Pdh {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}
