//! The collector and icon threads, and the snapshot they hand to the UI thread.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use suprvisor_core::{Collector, Proc, System};
use windows::Win32::Foundation::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::{WM_APP_DATA, WM_APP_ICONS, shell, trace};

/// What the collector thread hands to the UI thread.
#[derive(Clone, Default)]
pub struct Snapshot {
    pub system: System,
    pub procs: Arc<Vec<Proc>>,
    pub titles: Arc<HashMap<u32, String>>,
    pub icons: Arc<HashMap<String, Arc<shell::Icon>>>,
    pub history: Arc<VecDeque<System>>, // one sample per second, oldest first
}

pub enum Signal {
    Visible(bool),
    Rescan,      // a process was ended: list again now
    Table(bool), // the process table is on screen (else the Performance tab)
}

const SCAN: Duration = Duration::from_secs(3);
const FULL: Duration = Duration::from_secs(60);
const HISTORY: usize = 60; // seconds on the Performance graphs

/// The collector thread's state between ticks.
struct Collect {
    collector: Collector,
    hwnd: usize, // HWND is not Send; the value is valid on any thread
    data: Arc<Mutex<Snapshot>>,
    wanted: Arc<Mutex<Vec<(u32, i64)>>>,
    icons: mpsc::Sender<String>,
    procs: Vec<Proc>,
    index: HashMap<(u32, i64), usize>,
    snap: Snapshot,
    tried: HashSet<String>,
    history: VecDeque<System>,
    shown: bool,
    table: bool,
    ticks: u64,
    primed: bool,
    quick: bool, // the next list skips the kernel list
    scanned: Option<Instant>,
    full: Option<Instant>,
    engines: Option<Instant>,
    sampled: Option<Instant>,
}

/// Collector thread. Hidden, or on the Performance tab: machine totals once per second (the graph
/// history moves once per second, and each repaint costs ~4 ms of CPU). With the process table on
/// screen: totals every 500 ms, and every 500 ms the processes
/// under the visible rows; every 3 s the process list, window titles, new icons; every 60 s the
/// kernel list (per-process disk I/O). See suprvisor-core.
/// Once at start, hidden or not, the full list: the first show then has rows at once. Shown at
/// start (`fast`), that first list is the quick handle scan (~20 ms), so the window shows full
/// sooner. Icons load on their own thread and come in as they load, apps first.
pub fn spawn(hwnd: HWND, data: Arc<Mutex<Snapshot>>, wanted: Arc<Mutex<Vec<(u32, i64)>>>, signal: mpsc::Receiver<Signal>, table: bool, fast: bool) {
    let hwnd = hwnd.0 as usize;
    let icons = spawn_icons(hwnd, data.clone());
    std::thread::spawn(move || {
        // The process runs at high priority so the window stays smooth under full load; the
        // collector can wait. LOWEST in the high class is still above normal-priority processes.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST) };
        let Ok(collector) = Collector::new() else { return };
        let mut s = Collect {
            collector,
            hwnd,
            data,
            wanted,
            icons,
            procs: Vec::new(),
            index: HashMap::new(),
            snap: Snapshot::default(),
            tried: HashSet::new(),
            history: VecDeque::with_capacity(HISTORY),
            shown: false,
            table,
            ticks: 0,
            primed: false,
            quick: fast,
            scanned: None,
            full: None,
            engines: None,
            sampled: None,
        };
        let mut next = Instant::now();
        loop {
            let now = match signal.recv_timeout(next.saturating_duration_since(Instant::now())) {
                Ok(sig) => s.on_signal(sig),
                Err(RecvTimeoutError::Timeout) => true,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            if !now {
                continue;
            }
            s.tick();
            next = Instant::now() + Duration::from_millis(if s.shown && s.table { 500 } else { 1000 });
        }
    });
}

impl Collect {
    /// Takes a signal. Returns whether it asks for a tick now.
    fn on_signal(&mut self, signal: Signal) -> bool {
        match signal {
            Signal::Visible(v) => {
                // The list is stale after a hidden period: the next scan (now, if the last one is
                // older than SCAN) skips the slow kernel list. Not a forced scan: a show/hide
                // spammer would otherwise keep this thread scanning.
                let now = v && !self.shown;
                (self.quick, self.shown) = (self.quick || now, v);
                now // else a resize step or a hide: the next tick is soon enough
            }
            Signal::Table(t) => {
                let now = t && !self.table; // not listed while the Performance tab was open
                (self.quick, self.table) = (self.quick || now, t);
                now
            }
            Signal::Rescan => {
                self.scanned = None;
                true
            }
        }
    }

    fn tick(&mut self) {
        let listing = (self.shown && self.table) || !self.primed;
        self.ticks += 1;
        if !listing {
        } else if self.scanned.is_none_or(|t| t.elapsed() >= SCAN) {
            self.list();
        } else {
            self.refresh();
        }
        self.sample();
        if listing {
            self.snap.procs = Arc::new(self.procs.clone());
        }
        self.publish();
    }

    /// The process list, window titles, and new exe paths for the icon thread.
    fn list(&mut self) {
        let quick = std::mem::take(&mut self.quick);
        let t = Instant::now();
        self.procs = if !quick && self.full.is_none_or(|t| t.elapsed() >= FULL) {
            self.full = Some(Instant::now());
            let p = self.collector.scan_full().unwrap_or_else(|_| self.collector.scan());
            trace::record("scan_full", t, p.len() as u64);
            p
        } else {
            let p = self.collector.scan();
            trace::record("scan", t, p.len() as u64);
            p
        };
        self.scanned = Some(Instant::now());
        let t = Instant::now();
        self.index = self.procs.iter().enumerate().map(|(i, p)| ((p.pid, p.create_time), i)).collect();
        self.snap.titles = Arc::new(shell::window_titles());
        // New exe paths go to the icon thread, apps (a window title) first.
        let mut new: Vec<(bool, String)> = self
            .procs
            .iter()
            .filter(|p| !p.path.is_empty() && self.tried.insert(p.path.clone()))
            .map(|p| (!self.snap.titles.contains_key(&p.pid), p.path.clone()))
            .collect();
        new.sort();
        for (_, path) in new {
            let _ = self.icons.send(path);
        }
        trace::record("titles", t, self.snap.titles.len() as u64);
    }

    /// The processes under the visible rows.
    fn refresh(&mut self) {
        // Disk I/O is slow to read (~150 k cycles per process): every 3rd tick.
        let io = self.ticks % 3 == 0;
        let keys = self.wanted.lock().unwrap().clone(); // do not hold the lock: the UI thread writes it
        let t = Instant::now();
        for k in &keys {
            if let Some(&i) = self.index.get(k) {
                self.collector.refresh(std::slice::from_mut(&mut self.procs[i]), io);
            }
        }
        trace::record("refresh", t, keys.len() as u64);
    }

    /// Machine totals, and once per second a point of history.
    fn sample(&mut self) {
        // Idle GPU engines cost ~10 ms to read: every 3 s while visible, every 60 s while hidden.
        let all = self.engines.is_none_or(|t| t.elapsed() >= if self.shown { SCAN } else { FULL });
        if all {
            self.engines = Some(Instant::now());
        }
        let t = Instant::now();
        self.snap.system = self.collector.system(all);
        trace::record("system", t, all as u64);
        if self.sampled.is_none_or(|t| t.elapsed() >= Duration::from_millis(900)) {
            self.sampled = Some(Instant::now());
            if self.history.len() == HISTORY {
                self.history.pop_front();
            }
            self.history.push_back(self.snap.system.clone());
            self.snap.history = Arc::new(self.history.clone());
        }
    }

    fn publish(&mut self) {
        {
            let mut d = self.data.lock().unwrap();
            self.snap.icons = d.icons.clone(); // the icon thread's
            *d = self.snap.clone();
        }
        if self.shown || !std::mem::replace(&mut self.primed, true) {
            let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_APP_DATA, WPARAM(0), LPARAM(0)) };
        }
    }
}

/// Icon thread: loads the icon of each exe path it gets (~1-5 ms each; ~1-2 s for all at start)
/// and publishes them in batches, so the rows fill in while the rest load.
fn spawn_icons(hwnd: usize, data: Arc<Mutex<Snapshot>>) -> mpsc::Sender<String> {
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST) };
        let mut icons: HashMap<String, Arc<shell::Icon>> = HashMap::new();
        while let Ok(first) = rx.recv() {
            let t = Instant::now();
            let mut path = Some(first);
            while let Some(p) = path {
                if let Some(i) = shell::icon(&p) {
                    icons.insert(p, Arc::new(i));
                }
                path = if t.elapsed() < Duration::from_millis(50) { rx.try_recv().ok() } else { None };
            }
            data.lock().unwrap().icons = Arc::new(icons.clone());
            trace::record("icons", t, icons.len() as u64);
            let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_ICONS, WPARAM(0), LPARAM(0)) };
        }
    });
    tx
}
