//! Per-process network bytes, from the kernel's own TCP and UDP events (ETW provider
//! Microsoft-Windows-Kernel-Network), as Task Manager does. Needs admin: without it, every process reads 0.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Diagnostics::Etw::{
    CONTROLTRACE_HANDLE, ControlTraceW, EVENT_CONTROL_CODE_ENABLE_PROVIDER, EVENT_RECORD, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_LOGFILEW,
    EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE, EnableTraceEx2, OpenTraceW, PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_REAL_TIME,
    ProcessTrace, StartTraceW, WNODE_FLAG_TRACED_GUID,
};
use windows::core::{GUID, HSTRING, PWSTR};

const KERNEL_NETWORK: GUID = GUID::from_u128(0x7dd42a49_5329_4832_8dfd_43d979153a88);
const KEYWORD_IPV4_AND_IPV6: u64 = 0x10 | 0x20;
const LEVEL_INFORMATION: u8 = 4;

/// Bytes received and sent per pid.
type Counts = HashMap<u32, [u64; 2]>;

/// The window a rate is averaged over. Events arrive in bursts as the kernel flushes its buffers.
const WINDOW: Duration = Duration::from_secs(1);

pub struct Net {
    counts: Arc<Mutex<Counts>>, // since the last window, written by the trace thread
    rates: Counts,
    at: Instant,
}

impl Net {
    pub fn new() -> Self {
        let counts = Arc::new(Mutex::new(Counts::new()));
        let shared = counts.clone();
        std::thread::spawn(move || listen(&shared));
        Self { counts, rates: Counts::new(), at: Instant::now() }
    }

    /// Received and sent bytes per second of `pid`, over the last full window.
    pub fn rate(&self, pid: u32) -> [u64; 2] {
        self.rates.get(&pid).copied().unwrap_or_default()
    }

    /// Closes the current window when it is long enough.
    pub fn update(&mut self) {
        let secs = self.at.elapsed().as_secs_f64();
        if secs < WINDOW.as_secs_f64() {
            return;
        }
        let counts = std::mem::take(&mut *self.counts.lock().unwrap());
        self.rates = counts.into_iter().map(|(pid, bytes)| (pid, bytes.map(|b| (b as f64 / secs) as u64))).collect();
        self.at = Instant::now();
    }
}

/// One session per executable, so a test run does not stop the resident app's session.
fn session_name() -> HSTRING {
    let exe = std::env::current_exe().ok().and_then(|p| Some(p.file_stem()?.to_string_lossy().into_owned())).unwrap_or_default();
    HSTRING::from(format!("Suprvisor network {exe}"))
}

/// EVENT_TRACE_PROPERTIES with room after it for the session name, which the API writes there.
#[repr(C)]
struct Properties {
    header: EVENT_TRACE_PROPERTIES,
    name: [u16; 256],
}

fn properties() -> Properties {
    let mut properties = Properties { header: EVENT_TRACE_PROPERTIES::default(), name: [0; 256] };
    properties.header.Wnode.BufferSize = size_of::<Properties>() as u32;
    properties.header.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
    properties.header.Wnode.ClientContext = 1; // QueryPerformanceCounter time stamps
    properties.header.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
    properties.header.FlushTimer = 1;
    properties.header.LoggerNameOffset = size_of::<EVENT_TRACE_PROPERTIES>() as u32;
    properties
}

/// Runs the trace on this thread until the session stops. Returns at once without admin.
fn listen(counts: &Arc<Mutex<Counts>>) {
    let name = session_name();
    // A session left by a run that did not exit cleanly holds the name: stop it first.
    let mut stale = properties();
    let _ = unsafe { ControlTraceW(CONTROLTRACE_HANDLE::default(), &name, &mut stale.header, EVENT_TRACE_CONTROL_STOP) };
    let mut session = CONTROLTRACE_HANDLE::default();
    let mut started = properties();
    if unsafe { StartTraceW(&mut session, &name, &mut started.header) } != ERROR_SUCCESS {
        return;
    }
    let enable = EVENT_CONTROL_CODE_ENABLE_PROVIDER.0;
    if unsafe { EnableTraceEx2(session, &KERNEL_NETWORK, enable, LEVEL_INFORMATION, KEYWORD_IPV4_AND_IPV6, 0, 0, None) } != ERROR_SUCCESS {
        return;
    }
    let mut logfile = EVENT_TRACE_LOGFILEW { LoggerName: PWSTR(name.as_ptr().cast_mut()), Context: Arc::as_ptr(counts).cast_mut().cast(), ..Default::default() };
    logfile.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
    logfile.Anonymous2.EventRecordCallback = Some(on_event);
    let trace = unsafe { OpenTraceW(&mut logfile) };
    // ponytail: the session stops only when the process exits cleanly or a later run starts.
    let _ = unsafe { ProcessTrace(&[trace], None, None) };
}

/// TCP and UDP send and receive events. Their payload starts with the pid and the byte count.
unsafe extern "system" fn on_event(record: *mut EVENT_RECORD) {
    let record = unsafe { &*record };
    let direction = match record.EventHeader.EventDescriptor.Id {
        11 | 27 | 43 | 59 => 0, // received: TCP IPv4, TCP IPv6, UDP IPv4, UDP IPv6
        10 | 26 | 42 | 58 => 1, // sent
        _ => return,
    };
    if record.UserDataLength < 8 {
        return;
    }
    let payload = unsafe { std::slice::from_raw_parts(record.UserData.cast::<u8>(), 8) };
    let pid = u32::from_le_bytes(payload[0..4].try_into().unwrap());
    let size = u32::from_le_bytes(payload[4..8].try_into().unwrap());
    let counts = unsafe { &*record.UserContext.cast::<Mutex<Counts>>() };
    counts.lock().unwrap().entry(pid).or_default()[direction] += size as u64;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs admin: cargo test -- --ignored in an elevated shell"]
    fn counts_own_download() {
        let mut net = Net::new();
        std::thread::sleep(Duration::from_millis(500)); // the session starts
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let sender = std::thread::spawn(move || {
            use std::io::Write;
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&vec![0u8; 4_000_000]).unwrap();
        });
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        std::io::copy(&mut stream, &mut std::io::sink()).unwrap();
        sender.join().unwrap();
        std::thread::sleep(Duration::from_millis(2500)); // the kernel flushes its buffers each second
        net.update();
        let [received, sent] = net.rate(std::process::id());
        assert!(received > 1_000_000 && sent > 1_000_000, "received {received}, sent {sent}");
    }
}
