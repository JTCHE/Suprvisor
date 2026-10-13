//! Timing log for performance work, off unless SUPRVISOR_TRACE names a file. Written at exit, one
//! line per event: start in microseconds since launch, event, duration in microseconds, a count.
//! The first line is `epoch_us <launch in microseconds since 1970>`, to line events up with a driver.
//! audit/stress.ps1 reads it.

use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct Log {
    start: Instant,
    epoch_us: u128,
    path: String,
    events: Vec<(u64, &'static str, u64, u64)>,
}

static LOG: OnceLock<Option<Mutex<Log>>> = OnceLock::new();

fn log() -> Option<&'static Mutex<Log>> {
    LOG.get_or_init(|| std::env::var("SUPRVISOR_TRACE").ok().map(|path| Mutex::new(Log { start: Instant::now(), epoch_us: std::time::UNIX_EPOCH.elapsed().map_or(0, |d| d.as_micros()), path, events: Vec::with_capacity(1 << 16) })))
        .as_ref()
}

/// Records `event`, which started at `since` and ends now.
pub fn record(event: &'static str, since: Instant, count: u64) {
    if let Some(l) = log() {
        let mut l = l.lock().unwrap();
        let at = since.saturating_duration_since(l.start).as_micros() as u64;
        l.events.push((at, event, since.elapsed().as_micros() as u64, count));
    }
}

pub fn flush() {
    let Some(l) = log() else { return };
    let mut l = l.lock().unwrap();
    let Ok(f) = std::fs::File::create(&l.path) else { return };
    let mut f = std::io::BufWriter::new(f);
    let _ = writeln!(f, "epoch_us {}", l.epoch_us);
    for (at, event, us, n) in l.events.drain(..) {
        let _ = writeln!(f, "{at} {event} {us} {n}");
    }
}
