//! GPU engine use and dedicated VRAM, per process and per adapter, from D3DKMT statistics
//! (the kernel graphics counters behind Task Manager). PDH exposes the same data, but its
//! "GPU Engine" set has thousands of instances and costs hundreds of ms per collect.
//!
//! Per process this needs a handle with `PROCESS_QUERY_INFORMATION` (the limited right gives
//! STATUS_INVALID_PARAMETER). Without admin, processes of other users and services give no GPU data.

use std::time::Instant;

use windows::Wdk::Graphics::Direct3D::*;
use windows::Win32::Foundation::{HANDLE, LUID};

struct Adapter {
    luid: LUID,
    /// Segment ids of dedicated (non-aperture) memory.
    local_segments: Vec<u32>,
    nodes: u32,
    /// Per node: last global running time (100 ns), when it was read, and its last busy %.
    node_prev: Vec<Option<(i64, Instant, f32)>>,
}

/// Per-node running times (100 ns units) at the previous read, per adapter. Empty = no rate yet.
#[derive(Default)]
pub struct NodeTimes(Vec<Vec<i64>>);

pub struct Adapters {
    adapters: Vec<Adapter>,
}

fn query(q: &mut D3DKMT_QUERYSTATISTICS) -> bool {
    unsafe { D3DKMTQueryStatistics(q) }.is_ok()
}

fn new_query(kind: D3DKMT_QUERYSTATISTICS_TYPE, luid: LUID, process: HANDLE) -> D3DKMT_QUERYSTATISTICS {
    D3DKMT_QUERYSTATISTICS { Type: kind, AdapterLuid: luid, hProcess: process, ..Default::default() }
}

impl Adapters {
    pub fn new() -> Self {
        let mut e = D3DKMT_ENUMADAPTERS2::default();
        let _ = unsafe { D3DKMTEnumAdapters2(&mut e) }; // first call: count
        let mut infos = vec![D3DKMT_ADAPTERINFO::default(); e.NumAdapters as usize];
        e.pAdapters = infos.as_mut_ptr();
        if unsafe { D3DKMTEnumAdapters2(&mut e) }.is_err() {
            infos.clear();
        }
        infos.truncate(e.NumAdapters as usize);

        let mut adapters = Vec::new();
        for info in &infos {
            let _ = unsafe { D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER { hAdapter: info.hAdapter }) };
            let mut q = new_query(D3DKMT_QUERYSTATISTICS_ADAPTER, info.AdapterLuid, HANDLE::default());
            if !query(&mut q) {
                continue;
            }
            let a = unsafe { q.QueryResult.AdapterInformation };
            let local_segments: Vec<u32> = (0..a.NbSegments)
                .filter(|&id| {
                    let mut q = new_query(D3DKMT_QUERYSTATISTICS_SEGMENT, info.AdapterLuid, HANDLE::default());
                    q.Anonymous.QuerySegment.SegmentId = id;
                    query(&mut q) && unsafe { q.QueryResult.SegmentInformation.Aperture } == 0
                })
                .collect();
            if local_segments.is_empty() {
                continue; // software adapter: no dedicated memory
            }
            adapters.push(Adapter { luid: info.AdapterLuid, local_segments, nodes: a.NodeCount, node_prev: vec![None; a.NodeCount as usize] });
        }
        Self { adapters }
    }

    /// Whole machine: (busiest engine %, resident dedicated VRAM bytes).
    /// A global engine query costs 1-2.6 M cycles (the kernel sums over all processes), so with
    /// `all_engines` false only the engines that were busy at the last full read are read again.
    /// An engine that starts working shows up at the next full read.
    pub fn system(&mut self, all_engines: bool) -> (f32, u64) {
        let now = Instant::now();
        let (mut gpu, mut vram) = (0f32, 0u64);
        for a in &mut self.adapters {
            for (node, prev) in a.node_prev.iter_mut().enumerate() {
                let hot = prev.is_some_and(|(_, _, busy)| busy > 0.5);
                if !all_engines && !hot && prev.is_some() {
                    continue;
                }
                let mut q = new_query(D3DKMT_QUERYSTATISTICS_NODE, a.luid, HANDLE::default());
                q.Anonymous.QueryNode.NodeId = node as u32;
                if !query(&mut q) {
                    continue;
                }
                let time = unsafe { q.QueryResult.NodeInformation.GlobalInformation.RunningTime };
                let busy = prev.map_or(0.0, |(t, at, _)| busiest(&[t], &[time], (now - at).as_secs_f64() * 1e7));
                *prev = Some((time, now, busy));
                gpu = gpu.max(busy);
            }
            for &id in &a.local_segments {
                let mut q = new_query(D3DKMT_QUERYSTATISTICS_SEGMENT, a.luid, HANDLE::default());
                q.Anonymous.QuerySegment.SegmentId = id;
                if query(&mut q) {
                    vram += unsafe { q.QueryResult.SegmentInformation.BytesResident };
                }
            }
        }
        (gpu, vram)
    }

    /// One process: (busiest engine %, committed dedicated VRAM bytes). `elapsed_100ns` since `prev` was taken.
    pub fn process(&self, handle: HANDLE, prev: &mut NodeTimes, elapsed_100ns: f64) -> (f32, u64) {
        let (mut gpu, mut vram) = (0f32, 0u64);
        prev.0.resize(self.adapters.len(), Vec::new());
        for (a, prev) in self.adapters.iter().zip(prev.0.iter_mut()) {
            // One cheap call tells if the process has a device on this adapter at all.
            let mut q = new_query(D3DKMT_QUERYSTATISTICS_PROCESS_ADAPTER, a.luid, handle);
            if !query(&mut q) || unsafe { q.QueryResult.ProcessAdapterInformation.NodeCount } == 0 {
                prev.clear();
                continue;
            }
            for &id in &a.local_segments {
                let mut q = new_query(D3DKMT_QUERYSTATISTICS_PROCESS_SEGMENT, a.luid, handle);
                q.Anonymous.QueryProcessSegment.SegmentId = id;
                if query(&mut q) {
                    vram += unsafe { q.QueryResult.ProcessSegmentInformation.BytesCommitted };
                }
            }
            let cur: Vec<i64> = (0..a.nodes)
                .map(|node| {
                    let mut q = new_query(D3DKMT_QUERYSTATISTICS_PROCESS_NODE, a.luid, handle);
                    q.Anonymous.QueryProcessNode.NodeId = node;
                    if query(&mut q) { unsafe { q.QueryResult.ProcessNodeInformation.RunningTime } } else { 0 }
                })
                .collect();
            gpu = gpu.max(busiest(prev, &cur, elapsed_100ns));
            *prev = cur;
        }
        (gpu, vram)
    }
}

/// Highest per-node busy share between two running-time samples, 0-100.
fn busiest(prev: &[i64], cur: &[i64], elapsed_100ns: f64) -> f32 {
    if prev.len() != cur.len() || elapsed_100ns <= 0.0 {
        return 0.0;
    }
    prev.iter()
        .zip(cur)
        .map(|(p, c)| ((c - p) as f64 / elapsed_100ns * 100.0).clamp(0.0, 100.0))
        .fold(0.0, f64::max) as f32
}

#[test]
fn busiest_node_wins() {
    assert_eq!(busiest(&[0, 0], &[5_000_000, 2_500_000], 10_000_000.0), 50.0);
    assert_eq!(busiest(&[], &[1], 1.0), 0.0); // first sample: no rate yet
}

