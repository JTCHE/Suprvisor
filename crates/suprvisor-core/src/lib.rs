//! Process and system data collection for Suprvisor. No UI, so it can be tested and benchmarked headless.
//!
//! Four calls, four costs. The caller picks the rates (measured on a 24-thread machine with
//! 1,100 processes and 43,000 threads, in CPU cycles):
//! - [`Collector::system`]: machine totals. ~0.3 M + ~1.5 M per busy GPU engine. Every tick.
//! - [`Collector::refresh`]: CPU, RAM, GPU, VRAM of the given rows (the visible ones). ~10-80 k per row.
//! - [`Collector::scan`]: the process list, through one handle per process. ~60 M. Every few seconds.
//! - [`Collector::scan_full`]: `NtQuerySystemInformation`. ~250 M, because it copies every thread
//!   of every process. Gives per-process I/O, and the processes no handle can open. Rarely.

mod collector;
mod gpu;
mod handles;
mod io;
mod procs;
mod system;

pub use collector::Collector;

/// One process.
#[derive(Clone, Debug, Default)]
pub struct Proc {
    pub pid: u32,
    pub parent_pid: u32,
    /// Exe file name, for example `houdini.exe`.
    pub name: String,
    /// Full exe path. Empty for minimal processes (System, Registry) and processes no handle can open.
    pub path: String,
    /// Kernel `CreateTime`. With `pid`, identifies a process across calls (pids are reused).
    pub create_time: i64,
    /// Share of the whole machine, 0-100 (100 = all logical CPUs busy), like Task Manager.
    pub cpu: f32,
    /// Private working set in bytes. Task Manager's "Memory" column.
    pub private_ws: u64,
    /// Read and write bytes per second. All I/O (disk, network, devices), not disk only.
    /// Updated by [`Collector::scan_full`], and by [`Collector::refresh`] with `io`.
    pub io_read: u64,
    pub io_write: u64,
    /// Busiest GPU engine used by this process, 0-100.
    pub gpu: f32,
    /// Committed dedicated VRAM in bytes.
    pub vram: u64,
}

/// Machine totals.
#[derive(Clone, Debug, Default)]
pub struct System {
    /// 0-100, all logical CPUs.
    pub cpu: f32,
    /// 0-100 per logical processor.
    pub cores: Vec<f32>,
    pub ram_used: u64,
    pub ram_total: u64,
    /// Busiest GPU engine summed over all processes, 0-100.
    pub gpu: f32,
    pub vram_used: u64,
    pub vram_total: u64,
    /// Physical disk and network bytes per second, averaged since the previous read.
    pub disk_read: u64,
    pub disk_write: u64,
    pub net_recv: u64,
    pub net_sent: u64,
}

/// Ends the process, if `pid` still names the process created at `create_time`.
/// Ok when the process is already gone.
pub fn kill(pid: u32, create_time: i64) -> windows::core::Result<()> {
    handles::kill(pid, create_time)
}

/// The command line, for example `"C:\...\code.exe" --type=renderer`.
pub fn command_line(pid: u32) -> Option<String> {
    handles::command_line(pid)
}

/// True for processes marked critical: ending one stops Windows with a bug check.
pub fn is_critical(pid: u32) -> bool {
    handles::is_critical(pid)
}
