//! Builds the inventory from raw samples. Pure logic: no operating system calls.

use crate::model::{AdapterSample, GpuInfo, Luid, ProcessMeta, ProcessSample};
use serde::Serialize;
use std::collections::HashMap;

/// Name shown for a process that exited between the counter read and the process listing.
pub const UNKNOWN_PROCESS_NAME: &str = "<exited>";

/// Memory used by one process on one GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GpuUsage {
    pub luid: Luid,
    pub dedicated_bytes: u64,
    pub shared_bytes: u64,
    /// The counter reports more dedicated memory than the GPU physically has.
    /// Such values are shown but excluded from the attributed total.
    pub suspect: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProcessEntry {
    pub pid: u32,
    pub name: String,
    pub cmdline: String,
    pub parent_pid: Option<u32>,
    pub start_time: u64,
    pub usage: Vec<GpuUsage>,
}

impl ProcessEntry {
    pub fn dedicated_bytes(&self) -> u64 {
        self.usage.iter().map(|u| u.dedicated_bytes).sum()
    }

    pub fn shared_bytes(&self) -> u64 {
        self.usage.iter().map(|u| u.shared_bytes).sum()
    }

    pub fn is_suspect(&self) -> bool {
        self.usage.iter().any(|u| u.suspect)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GpuSummary {
    pub gpu: GpuInfo,
    pub used_bytes: u64,
    pub attributed_bytes: u64,
    /// Used memory that no process accounts for (driver and system reservations).
    pub unattributed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Inventory {
    pub gpus: Vec<GpuSummary>,
    pub processes: Vec<ProcessEntry>,
}

/// Several processes sharing one executable name, aggregated for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppGroup {
    pub name: String,
    pub process_count: usize,
    pub dedicated_bytes: u64,
    pub shared_bytes: u64,
}

pub fn build_inventory(
    gpus: &[GpuInfo],
    adapter_samples: &[AdapterSample],
    process_samples: &[ProcessSample],
    metas: &HashMap<u32, ProcessMeta>,
) -> Inventory {
    let totals: HashMap<Luid, u64> = gpus.iter().map(|g| (g.luid, g.total_bytes)).collect();

    let mut by_pid: HashMap<u32, Vec<GpuUsage>> = HashMap::new();
    for sample in process_samples {
        let suspect = totals
            .get(&sample.luid)
            .is_some_and(|total| sample.dedicated_bytes > *total);
        by_pid.entry(sample.pid).or_default().push(GpuUsage {
            luid: sample.luid,
            dedicated_bytes: sample.dedicated_bytes,
            shared_bytes: sample.shared_bytes,
            suspect,
        });
    }

    let mut processes: Vec<ProcessEntry> = by_pid
        .into_iter()
        .map(|(pid, mut usage)| {
            usage.sort_by_key(|u| u.luid);
            let meta = metas.get(&pid);
            ProcessEntry {
                pid,
                name: meta.map_or_else(|| UNKNOWN_PROCESS_NAME.to_string(), |m| m.name.clone()),
                cmdline: meta.map_or_else(String::new, |m| m.cmdline.clone()),
                parent_pid: meta.and_then(|m| m.parent_pid),
                start_time: meta.map_or(0, |m| m.start_time),
                usage,
            }
        })
        .collect();

    // Reliable readings first, biggest first; suspect readings last. PID breaks ties.
    processes.sort_by(|a, b| {
        a.is_suspect()
            .cmp(&b.is_suspect())
            .then(b.dedicated_bytes().cmp(&a.dedicated_bytes()))
            .then(a.pid.cmp(&b.pid))
    });

    let summaries = gpus
        .iter()
        .map(|gpu| {
            let used_bytes = adapter_samples
                .iter()
                .find(|s| s.luid == gpu.luid)
                .map_or(0, |s| s.dedicated_used_bytes);
            let attributed_bytes: u64 = processes
                .iter()
                .flat_map(|p| p.usage.iter())
                .filter(|u| u.luid == gpu.luid && !u.suspect)
                .map(|u| u.dedicated_bytes)
                .sum();
            GpuSummary {
                gpu: gpu.clone(),
                used_bytes,
                attributed_bytes,
                unattributed_bytes: used_bytes.saturating_sub(attributed_bytes),
            }
        })
        .collect();

    Inventory {
        gpus: summaries,
        processes,
    }
}

/// Aggregates processes by executable name (case-insensitive), biggest group first.
pub fn group_by_app(processes: &[ProcessEntry]) -> Vec<AppGroup> {
    let mut groups: HashMap<String, AppGroup> = HashMap::new();
    for p in processes {
        let group = groups
            .entry(p.name.to_lowercase())
            .or_insert_with(|| AppGroup {
                name: p.name.clone(),
                process_count: 0,
                dedicated_bytes: 0,
                shared_bytes: 0,
            });
        group.process_count += 1;
        group.dedicated_bytes += p.dedicated_bytes();
        group.shared_bytes += p.shared_bytes();
    }
    let mut out: Vec<AppGroup> = groups.into_values().collect();
    out.sort_by(|a, b| {
        b.dedicated_bytes
            .cmp(&a.dedicated_bytes)
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    fn gpu(index: usize, low: u32, total_gib: u64) -> GpuInfo {
        GpuInfo {
            index,
            name: format!("GPU {index}"),
            luid: Luid::new(0, low),
            total_bytes: total_gib * GIB,
        }
    }

    fn sample(pid: u32, low: u32, dedicated: u64, shared: u64) -> ProcessSample {
        ProcessSample {
            pid,
            luid: Luid::new(0, low),
            dedicated_bytes: dedicated,
            shared_bytes: shared,
        }
    }

    fn meta(name: &str) -> ProcessMeta {
        ProcessMeta {
            name: name.to_string(),
            cmdline: format!("{name} --flag"),
            parent_pid: Some(1),
            start_time: 100,
        }
    }

    #[test]
    fn computes_unattributed_memory_per_gpu() {
        let gpus = [gpu(0, 1, 24)];
        let adapters = [AdapterSample {
            luid: Luid::new(0, 1),
            dedicated_used_bytes: 10 * GIB,
        }];
        let samples = [sample(10, 1, 6 * GIB, 0), sample(11, 1, 3 * GIB, 0)];
        let inv = build_inventory(&gpus, &adapters, &samples, &HashMap::new());
        let summary = &inv.gpus[0];
        assert_eq!(summary.used_bytes, 10 * GIB);
        assert_eq!(summary.attributed_bytes, 9 * GIB);
        assert_eq!(summary.unattributed_bytes, GIB);
    }

    #[test]
    fn unattributed_never_goes_negative() {
        let gpus = [gpu(0, 1, 24)];
        let adapters = [AdapterSample {
            luid: Luid::new(0, 1),
            dedicated_used_bytes: 2 * GIB,
        }];
        let samples = [sample(10, 1, 3 * GIB, 0)];
        let inv = build_inventory(&gpus, &adapters, &samples, &HashMap::new());
        assert_eq!(inv.gpus[0].unattributed_bytes, 0);
    }

    #[test]
    fn flags_readings_above_the_physical_vram_and_excludes_them_from_the_total() {
        let gpus = [gpu(0, 1, 24)];
        let adapters = [AdapterSample {
            luid: Luid::new(0, 1),
            dedicated_used_bytes: 20 * GIB,
        }];
        let samples = [sample(1, 1, 55 * GIB, 0), sample(2, 1, 15 * GIB, 0)];
        let inv = build_inventory(&gpus, &adapters, &samples, &HashMap::new());
        let suspect = inv.processes.iter().find(|p| p.pid == 1).unwrap();
        assert!(suspect.is_suspect());
        assert_eq!(inv.gpus[0].attributed_bytes, 15 * GIB);
        assert_eq!(inv.gpus[0].unattributed_bytes, 5 * GIB);
    }

    #[test]
    fn sorts_reliable_readings_by_size_and_suspect_readings_last() {
        let gpus = [gpu(0, 1, 24)];
        let samples = [
            sample(1, 1, 55 * GIB, 0),
            sample(2, 1, 100 * MIB, 0),
            sample(3, 1, 15 * GIB, 0),
        ];
        let inv = build_inventory(&gpus, &[], &samples, &HashMap::new());
        let order: Vec<u32> = inv.processes.iter().map(|p| p.pid).collect();
        assert_eq!(order, vec![3, 2, 1]);
    }

    #[test]
    fn merges_samples_of_one_process_across_gpus() {
        let gpus = [gpu(0, 1, 24), gpu(1, 2, 12)];
        let samples = [sample(7, 1, 2 * GIB, 0), sample(7, 2, GIB, 5 * MIB)];
        let inv = build_inventory(&gpus, &[], &samples, &HashMap::new());
        assert_eq!(inv.processes.len(), 1);
        let p = &inv.processes[0];
        assert_eq!(p.usage.len(), 2);
        assert_eq!(p.dedicated_bytes(), 3 * GIB);
        assert_eq!(p.shared_bytes(), 5 * MIB);
    }

    #[test]
    fn enriches_with_metadata_and_names_exited_processes() {
        let gpus = [gpu(0, 1, 24)];
        let samples = [sample(10, 1, GIB, 0), sample(11, 1, MIB, 0)];
        let mut metas = HashMap::new();
        metas.insert(10, meta("llama-server.exe"));
        let inv = build_inventory(&gpus, &[], &samples, &metas);
        let known = inv.processes.iter().find(|p| p.pid == 10).unwrap();
        assert_eq!(known.name, "llama-server.exe");
        assert_eq!(known.cmdline, "llama-server.exe --flag");
        assert_eq!(known.parent_pid, Some(1));
        let gone = inv.processes.iter().find(|p| p.pid == 11).unwrap();
        assert_eq!(gone.name, UNKNOWN_PROCESS_NAME);
    }

    #[test]
    fn keeps_usage_on_unknown_adapters_without_a_suspect_flag() {
        let gpus = [gpu(0, 1, 24)];
        let samples = [sample(10, 99, 100 * GIB, 0)];
        let inv = build_inventory(&gpus, &[], &samples, &HashMap::new());
        assert!(!inv.processes[0].is_suspect());
        assert_eq!(inv.gpus[0].attributed_bytes, 0);
    }

    #[test]
    fn groups_processes_by_name_ignoring_case() {
        let gpus = [gpu(0, 1, 24)];
        let samples = [
            sample(1, 1, 300 * MIB, 0),
            sample(2, 1, 200 * MIB, 0),
            sample(3, 1, GIB, 0),
        ];
        let mut metas = HashMap::new();
        metas.insert(1, meta("msedgewebview2.exe"));
        metas.insert(2, meta("MSEdgeWebView2.exe"));
        metas.insert(3, meta("llama-server.exe"));
        let inv = build_inventory(&gpus, &[], &samples, &metas);
        let groups = group_by_app(&inv.processes);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "llama-server.exe");
        assert_eq!(groups[1].process_count, 2);
        assert_eq!(groups[1].dedicated_bytes, 500 * MIB);
    }
}
