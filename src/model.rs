//! Core data types shared by the collectors, the inventory builder and the renderers.

use serde::Serialize;
use std::fmt;

/// Locally unique identifier of a GPU adapter, as used by Windows performance counters and DXGI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct Luid {
    pub high: u32,
    pub low: u32,
}

impl Luid {
    pub fn new(high: u32, low: u32) -> Self {
        Self { high, low }
    }
}

impl fmt::Display for Luid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:08x}_0x{:08x}", self.high, self.low)
    }
}

/// A GPU adapter detected on the machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GpuInfo {
    pub index: usize,
    pub name: String,
    pub luid: Luid,
    pub total_bytes: u64,
}

/// Raw per-process, per-adapter memory reading from the `GPU Process Memory` counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessSample {
    pub pid: u32,
    pub luid: Luid,
    pub dedicated_bytes: u64,
    pub shared_bytes: u64,
}

/// Raw per-adapter reading from the `GPU Adapter Memory` counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterSample {
    pub luid: Luid,
    pub dedicated_used_bytes: u64,
}

/// Process details gathered from the operating system, used to enrich the samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessMeta {
    pub name: String,
    pub cmdline: String,
    pub parent_pid: Option<u32>,
    pub start_time: u64,
}

/// Extra details about one running process, read on demand for the details popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessDetails {
    /// Full path of the executable, when readable.
    pub exe: Option<String>,
    pub cmdline: String,
    /// Working directory, when readable.
    pub cwd: Option<String>,
    /// Parent PID and its name, when the parent is still running.
    pub parent: Option<(u32, String)>,
    /// Account the process runs under, when readable.
    pub user: Option<String>,
    /// Working-set memory (RAM), in bytes.
    pub ram_bytes: u64,
}
