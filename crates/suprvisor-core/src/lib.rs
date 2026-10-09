//! Process and system data collection for Suprvisor. No UI, so it can be tested and benchmarked headless.
//!
//! Four calls, four costs. The caller picks the rates (measured on a 24-thread machine with
//! 1,100 processes and 43,000 threads, in CPU cycles):
//! - [`Collector::system`]: machine totals. ~0.3 M + ~1.5 M per busy GPU engine. Every tick.
//! - [`Collector::refresh`]: CPU, RAM, GPU, VRAM of the given rows (the visible ones). ~10-80 k per row.
//! - [`Collector::scan`]: the process list, through one handle per process. ~60 M. Every few seconds.
//! - [`Collector::scan_full`]: `NtQuerySystemInformation`. ~250 M, because it copies every thread
//!   of every process. Gives per-process I/O, and the processes no handle can open. Rarely.

mod gpu;
mod handles;
mod io;
mod procs;
mod system;

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use windows::Win32::Foundation::HANDLE;

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

/// Per-process state kept between calls: last values, the handle, and the counters rates come from.
struct Tracked {
    p: Proc,
    handle: HANDLE, // invalid = no handle can open it: only scan_full updates it
    gpu_ok: bool,   // handle has PROCESS_QUERY_INFORMATION, which D3DKMT needs
    at: Instant,    // when cpu_time and gpu_nodes were read
    cpu_time: i64,  // kernel + user, 100 ns units
    io_at: Option<Instant>,
    read: u64,
    write: u64,
    gpu_nodes: gpu::NodeTimes,
    seen: u64, // last scan generation that saw it
}

impl Drop for Tracked {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            handles::close(self.handle);
        }
    }
}

fn cpu_share(cpus: u32, busy_100ns: i64, secs: f64) -> f32 {
    (busy_100ns as f64 / 1e7 / secs / cpus as f64 * 100.0).clamp(0.0, 100.0) as f32
}

/// Shortest interval for a new rate. CPU times advance in clock ticks (15.6 ms), so a rate over
/// a few ms is noise: a row refreshed just before a scan keeps its rate until this much time passed.
const MIN_RATE_SECS: f64 = 0.25;

impl Tracked {
    /// New CPU time read at `now`: updates CPU, RAM, GPU and VRAM.
    fn update(&mut self, gpu: &gpu::Adapters, cpus: u32, cpu_time: i64, now: Instant) {
        if let Some(ws) = handles::private_ws(self.handle) {
            self.p.private_ws = ws;
        }
        let secs = (now - self.at).as_secs_f64();
        if secs < MIN_RATE_SECS {
            return;
        }
        self.p.cpu = cpu_share(cpus, cpu_time - self.cpu_time, secs);
        self.cpu_time = cpu_time;
        if self.gpu_ok {
            (self.p.gpu, self.p.vram) = gpu.process(self.handle, &mut self.gpu_nodes, secs * 1e7);
        }
        self.at = now;
    }

    fn update_io(&mut self, read: u64, write: u64, now: Instant) {
        if let Some(at) = self.io_at {
            let secs = (now - at).as_secs_f64();
            if secs < MIN_RATE_SECS {
                return;
            }
            self.p.io_read = (read.saturating_sub(self.read) as f64 / secs) as u64;
            self.p.io_write = (write.saturating_sub(self.write) as f64 / secs) as u64;
        }
        (self.read, self.write, self.io_at) = (read, write, Some(now));
    }
}

pub struct Collector {
    procs: procs::Procs,
    io: io::Io,
    gpu: gpu::Adapters,
    cpu_times: system::SystemTimes,
    tracked: HashMap<(u32, i64), Tracked>,
    generation: u64,
    cpus: u32,
    vram_total: u64,
}

// The handles are plain kernel handles, usable from any thread.
unsafe impl Send for Collector {}

impl Collector {
    pub fn new() -> windows::core::Result<Self> {
        Ok(Self {
            procs: procs::Procs::new(),
            io: io::Io::new(),
            gpu: gpu::Adapters::new(),
            cpu_times: system::SystemTimes::default(),
            tracked: HashMap::new(),
            generation: 0,
            cpus: system::logical_cpus(),
            vram_total: system::vram_total(),
        })
    }

    /// Machine totals. Rates are averages since the previous read (0 on the first call).
    /// Cheap enough to call every second while hidden (the Performance tab history).
    /// `all_gpu_engines`: also read the idle GPU engines (~10 ms), to see one that became busy.
    /// Without it, only the engines busy at the last full read are read (~20 us).
    pub fn system(&mut self, all_gpu_engines: bool) -> System {
        let (ram_used, ram_total) = system::memory();
        let (gpu, vram_used) = self.gpu.system(all_gpu_engines);
        let io = self.io.collect();
        System {
            cpu: self.cpu_times.cpu(),
            ram_used,
            ram_total,
            gpu,
            vram_used,
            vram_total: self.vram_total,
            disk_read: io.disk_read,
            disk_write: io.disk_write,
            net_recv: io.net_recv,
            net_sent: io.net_sent,
        }
    }

    /// The process list. New processes have zero rates until the next read.
    pub fn scan(&mut self) -> Vec<Proc> {
        self.generation += 1;
        let generation = self.generation;
        let now = Instant::now();
        for h in handles::walk() {
            let Some((create, cpu_time)) = handles::times(h) else {
                handles::close(h);
                continue;
            };
            let key = (handles::pid(h), create);
            if let Some(t) = self.tracked.get_mut(&key) {
                if t.handle.is_invalid() {
                    t.handle = h; // was unopenable at scan_full time, opens now
                } else {
                    handles::close(h);
                }
                t.seen = generation;
                t.update(&self.gpu, self.cpus, cpu_time, now);
                continue;
            }
            let mut t = self.track(key, h, now);
            t.cpu_time = cpu_time;
            t.seen = generation;
            self.tracked.insert(key, t);
        }
        // Exited processes drop here (closing their handles). Unopenable ones wait for scan_full.
        self.tracked.retain(|_, t| t.seen == generation || t.handle.is_invalid());
        self.tracked.values().map(|t| t.p.clone()).collect()
    }

    /// Full kernel list: per-process I/O for every process, and the processes that no handle can open.
    pub fn scan_full(&mut self) -> windows::core::Result<Vec<Proc>> {
        let raw = self.procs.query()?;
        let now = Instant::now();
        let mut alive = HashSet::with_capacity(raw.len());
        for r in raw {
            let key = (r.pid, r.create_time);
            alive.insert(key);
            if !self.tracked.contains_key(&key) {
                let h = handles::open_limited(r.pid).unwrap_or_default();
                let mut t = self.track(key, h, now);
                t.cpu_time = r.counters.cpu_time;
                t.p.parent_pid = r.parent_pid;
                self.tracked.insert(key, t);
            }
            let t = self.tracked.get_mut(&key).unwrap();
            t.update_io(r.counters.read, r.counters.write, now);
            if t.p.name.is_empty() {
                t.p.name = r.name; // minimal processes (Registry, Memory Compression) have no image path
            }
            if t.handle.is_invalid() {
                t.p.private_ws = r.private_ws;
                let secs = (now - t.at).as_secs_f64();
                if secs >= MIN_RATE_SECS {
                    t.p.cpu = cpu_share(self.cpus, r.counters.cpu_time - t.cpu_time, secs);
                    (t.cpu_time, t.at) = (r.counters.cpu_time, now);
                }
            }
        }
        self.tracked.retain(|k, _| alive.contains(k));
        Ok(self.tracked.values().map(|t| t.p.clone()).collect())
    }

    /// Updates CPU, RAM, GPU and VRAM of `rows` in place; with `io`, also I/O (slow, ~150 k cycles per row).
    /// Rows unknown to the last scan, or with no handle, are left as they are.
    pub fn refresh(&mut self, rows: &mut [Proc], io: bool) {
        let now = Instant::now();
        for p in rows {
            let Some(t) = self.tracked.get_mut(&(p.pid, p.create_time)) else { continue };
            if t.handle.is_invalid() {
                continue;
            }
            if let Some((_, cpu_time)) = handles::times(t.handle) {
                t.update(&self.gpu, self.cpus, cpu_time, now);
            }
            if io && let Some((r, w)) = handles::io(t.handle) {
                t.update_io(r, w, now);
            }
            (p.cpu, p.private_ws, p.gpu, p.vram) = (t.p.cpu, t.p.private_ws, t.p.gpu, t.p.vram);
            (p.io_read, p.io_write) = (t.p.io_read, t.p.io_write);
        }
    }

    /// State for a process seen for the first time, through `h` (may be invalid).
    fn track(&self, (pid, create): (u32, i64), h: HANDLE, now: Instant) -> Tracked {
        let mut t = Tracked {
            p: Proc { pid, create_time: create, ..Default::default() },
            handle: h,
            gpu_ok: false,
            at: now,
            cpu_time: 0,
            io_at: None,
            read: 0,
            write: 0,
            gpu_nodes: gpu::NodeTimes::default(),
            seen: 0,
        };
        if h.is_invalid() {
            return t;
        }
        // GPU stats need a stronger right than the walk uses. Check the pid was not reused meanwhile.
        if let Some(full) = handles::open_full(pid) {
            if handles::times(full).is_some_and(|(c, _)| c == create) {
                handles::close(h);
                (t.handle, t.gpu_ok) = (full, true);
            } else {
                handles::close(full);
            }
        }
        t.p.path = handles::path(t.handle).unwrap_or_default();
        t.p.name = t.p.path.rsplit('\\').next().unwrap_or_default().to_owned();
        t.p.parent_pid = handles::parent_pid(t.handle);
        t.p.private_ws = handles::private_ws(t.handle).unwrap_or(0);
        if t.gpu_ok {
            (_, t.p.vram) = self.gpu.process(t.handle, &mut t.gpu_nodes, 0.0);
        }
        t
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn burn(ms: u128) {
        let t = Instant::now();
        while t.elapsed().as_millis() < ms {
            std::hint::black_box((0..1000).sum::<u64>());
        }
    }

    #[test]
    fn collector_is_sane() {
        let mut c = Collector::new().unwrap();
        c.system(true);
        c.scan();
        burn(300);
        let mut all = c.scan();
        let s = c.system(true);

        let me = all.iter().position(|p| p.pid == std::process::id()).expect("own process listed");
        assert!(all[me].name.to_lowercase().starts_with("suprvisor_core"), "{}", all[me].name);
        assert!(all[me].cpu > 0.0 && all[me].private_ws > 0 && all[me].parent_pid != 0);
        let mut pids: Vec<_> = all.iter().map(|p| p.pid).collect();
        pids.sort_unstable();
        pids.dedup();
        assert_eq!(pids.len(), all.len(), "pids unique");
        assert!(all.iter().all(|p| (0.0..=100.0).contains(&p.cpu) && (0.0..=100.0).contains(&p.gpu)));
        assert!((0.0..=100.0).contains(&s.cpu));
        assert!(s.ram_used > 0 && s.ram_used < s.ram_total);
        assert!(s.vram_total > 0 && s.vram_used <= s.vram_total);
        assert!(all.iter().map(|p| p.private_ws).sum::<u64>() <= s.ram_total);

        burn(300);
        all[me].cpu = -1.0;
        c.refresh(&mut all[me..=me], true);
        assert!(all[me].cpu > 0.0, "refresh updates the row");

        // The kernel list agrees with the walk, and names the processes the walk cannot open.
        let full = c.scan_full().unwrap();
        assert!(full.len() >= all.len() * 9 / 10, "{} vs {}", full.len(), all.len());
        assert!(full.iter().all(|p| !p.name.is_empty()), "every process has a name");

        let me_proc = &all[me];
        assert!(me_proc.path.ends_with(&me_proc.name) && me_proc.path.contains('\\'));
        assert!(command_line(me_proc.pid).is_some_and(|c| c.contains("suprvisor_core")));
        assert!(!is_critical(me_proc.pid));
        assert!(kill(me_proc.pid, me_proc.create_time + 1).is_ok(), "a reused pid is not killed");
    }
}
