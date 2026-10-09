//! Headless collector benchmark: CPU cost of each call, plus the top processes by VRAM and CPU.
//!   cargo run -p suprvisor-core --release --example bench [ticks]
//! Cost is this thread's CPU cycles (QueryThreadCycleTime), so a loaded machine does not inflate it.
use std::time::{Duration, Instant};

use suprvisor_core::Collector;
use windows::Win32::System::{Threading::GetCurrentThread, WindowsProgramming::QueryThreadCycleTime};

fn cycles() -> u64 {
    let mut c = 0;
    let _ = unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut c) };
    c
}

fn thread_ms() -> f64 {
    use windows::Win32::{Foundation::FILETIME, System::Threading::GetThreadTimes};
    let (mut a, mut b, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    let _ = unsafe { GetThreadTimes(GetCurrentThread(), &mut a, &mut b, &mut k, &mut u) };
    let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f64 / 1e4;
    t(k) + t(u)
}

fn main() -> windows::core::Result<()> {
    let ticks: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(10);
    // Cycles per ms of CPU time: busy loop until this thread has run 500 ms. Thread time, not wall
    // time, so preemption on a loaded machine does not skew it.
    let (t0, c) = (thread_ms(), cycles());
    while thread_ms() - t0 < 500.0 {
        std::hint::black_box(0);
    }
    let per_ms = (cycles() - c) as f64 / (thread_ms() - t0);
    println!("calibration: {:.2} GHz", per_ms / 1e6);

    let (c0, t) = (cycles(), Instant::now());
    let mut col = Collector::new()?;
    println!("new: {:.1} ms CPU ({:.0} ms wall)", (cycles() - c0) as f64 / per_ms, t.elapsed().as_secs_f64() * 1e3);
    col.system(true);
    let mut all = col.scan();
    col.scan_full()?;

    let (mut sys, mut refresh, mut scan, mut full) = (vec![], vec![], vec![], vec![]);
    for tick in 0..ticks {
        std::thread::sleep(Duration::from_millis(500));
        let c = cycles();
        let s = col.system(false);
        sys.push((cycles() - c) as f64 / per_ms);
        all.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
        let c = cycles();
        col.refresh(&mut all[..30], false);
        refresh.push((cycles() - c) as f64 / per_ms);
        let c = cycles();
        all = col.scan();
        scan.push((cycles() - c) as f64 / per_ms);
        if tick % 3 == 0 {
            let c = cycles();
            col.scan_full()?;
            full.push((cycles() - c) as f64 / per_ms);
        }
        if tick == ticks - 1 {
            let gb = |b: u64| b as f64 / (1u64 << 30) as f64;
            println!(
                "system: CPU {:.0}%, RAM {:.1}/{:.1} GB, GPU {:.0}%, VRAM {:.1}/{:.1} GB, disk r/w {}/{} KB/s, net r/s {}/{} KB/s, {} processes",
                s.cpu, gb(s.ram_used), gb(s.ram_total), s.gpu, gb(s.vram_used), gb(s.vram_total),
                s.disk_read >> 10, s.disk_write >> 10, s.net_recv >> 10, s.net_sent >> 10, all.len()
            );
        }
    }
    let med = |v: &mut Vec<f64>| { v.sort_by(f64::total_cmp); v[v.len() / 2] };
    let (sys, refresh, scan, full) = (med(&mut sys), med(&mut refresh), med(&mut scan), med(&mut full));
    println!("CPU per call (median): system {sys:.3} ms, refresh(30 rows) {refresh:.3} ms, scan {scan:.2} ms, scan_full {full:.2} ms");
    println!("hidden cost: system every 1 s = {:.3}% of one core", sys / 1000.0 * 100.0);
    for (scan_s, full_s) in [(2.0, 30.0), (3.0, 30.0), (3.0, 60.0)] {
        let pct = (sys + refresh) / 500.0 * 100.0 + scan / (scan_s * 1e3) * 100.0 + full / (full_s * 1e3) * 100.0;
        println!("visible cost: 500 ms fast tick, scan every {scan_s} s, scan_full every {full_s} s = {pct:.2}% of one core");
    }

    all.sort_by_key(|p| std::cmp::Reverse(p.vram));
    println!("top VRAM:");
    for p in all.iter().take(6) {
        println!("  {:>6} {:<24} VRAM {:>6} MB  GPU {:>5.1}%", p.pid, p.name, p.vram >> 20, p.gpu);
    }
    all.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
    println!("top CPU:");
    for p in all.iter().take(6) {
        println!("  {:>6} {:<24} CPU {:>5.1}%  RAM {:>6} MB  IO r/w {}/{} KB/s", p.pid, p.name, p.cpu, p.private_ws >> 20, p.io_read >> 10, p.io_write >> 10);
    }
    Ok(())
}
