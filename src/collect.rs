//! Ties the Windows collectors together into one inventory. Read-only.

use crate::gpu::enumerate_gpus;
use crate::inventory::{Inventory, build_inventory};
use crate::pdh::{read_adapter_samples, read_process_samples};
use crate::procs::read_process_meta;
use anyhow::{Result, bail};

pub fn collect_inventory() -> Result<Inventory> {
    let gpus = enumerate_gpus()?;
    if gpus.is_empty() {
        bail!("no hardware GPU adapter found");
    }
    let adapter_samples = read_adapter_samples()?;
    let process_samples = read_process_samples()?;
    let metas = read_process_meta();
    Ok(build_inventory(
        &gpus,
        &adapter_samples,
        &process_samples,
        &metas,
    ))
}
