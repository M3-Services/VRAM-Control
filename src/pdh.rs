//! Reads GPU memory counters through the Windows Performance Data Helper (PDH) API.
//!
//! `nvidia-smi` cannot report per-process memory in WDDM mode, but Windows exposes it through
//! the `GPU Process Memory` and `GPU Adapter Memory` counter sets.

use crate::model::{AdapterSample, ProcessSample};
use crate::pdh_names::{parse_adapter_instance, parse_process_instance};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW,
};
use windows::core::PCWSTR;

const PROCESS_DEDICATED: &str = r"\GPU Process Memory(*)\Dedicated Usage";
const PROCESS_SHARED: &str = r"\GPU Process Memory(*)\Shared Usage";
const ADAPTER_DEDICATED: &str = r"\GPU Adapter Memory(*)\Dedicated Usage";

/// Closes the PDH query when dropped, so early returns never leak it.
struct Query(PDH_HQUERY);

impl Drop for Query {
    fn drop(&mut self) {
        // SAFETY: the handle comes from a successful PdhOpenQueryW and is closed once.
        unsafe {
            PdhCloseQuery(self.0);
        }
    }
}

fn check(status: u32, what: &str) -> Result<()> {
    if status != 0 {
        bail!("{what} failed (PDH status 0x{status:08x})");
    }
    Ok(())
}

/// Reads every instance of a wildcard counter path as `(instance name, value)` pairs.
fn read_counter_array(path: &str) -> Result<Vec<(String, i64)>> {
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: all pointers passed below point to live local data; the item buffer is kept alive
    // while its embedded name pointers are read.
    unsafe {
        let mut handle = PDH_HQUERY::default();
        check(
            PdhOpenQueryW(PCWSTR::null(), 0, &mut handle),
            "PdhOpenQueryW",
        )?;
        let query = Query(handle);

        let mut counter = PDH_HCOUNTER::default();
        check(
            PdhAddEnglishCounterW(query.0, PCWSTR(wide.as_ptr()), 0, &mut counter),
            "PdhAddEnglishCounterW",
        )
        .with_context(|| format!("counter path {path}"))?;
        check(PdhCollectQueryData(query.0), "PdhCollectQueryData")?;

        let mut size = 0u32;
        let mut count = 0u32;
        let status =
            PdhGetFormattedCounterArrayW(counter, PDH_FMT_LARGE, &mut size, &mut count, None);
        if status != PDH_MORE_DATA {
            check(status, "PdhGetFormattedCounterArrayW (size query)")?;
            return Ok(Vec::new());
        }

        // u64 elements keep the buffer 8-byte aligned, as the item structs require.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let items = buffer.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
        check(
            PdhGetFormattedCounterArrayW(
                counter,
                PDH_FMT_LARGE,
                &mut size,
                &mut count,
                Some(items),
            ),
            "PdhGetFormattedCounterArrayW",
        )?;

        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let item = &*items.add(i);
            if item.FmtValue.CStatus != 0 {
                continue;
            }
            let name = item.szName.to_string().unwrap_or_default();
            out.push((name, item.FmtValue.Anonymous.largeValue));
        }
        Ok(out)
    }
}

fn to_bytes(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Reads per-process dedicated and shared GPU memory, one sample per (process, adapter).
pub fn read_process_samples() -> Result<Vec<ProcessSample>> {
    let dedicated = read_counter_array(PROCESS_DEDICATED)?;
    let shared: HashMap<String, i64> = read_counter_array(PROCESS_SHARED)?.into_iter().collect();

    let mut samples = Vec::new();
    for (name, value) in dedicated {
        let Some(instance) = parse_process_instance(&name) else {
            continue;
        };
        samples.push(ProcessSample {
            pid: instance.pid,
            luid: instance.luid,
            dedicated_bytes: to_bytes(value),
            shared_bytes: shared.get(&name).copied().map_or(0, to_bytes),
        });
    }
    Ok(samples)
}

/// Reads the total dedicated memory in use on each adapter.
pub fn read_adapter_samples() -> Result<Vec<AdapterSample>> {
    let mut samples = Vec::new();
    for (name, value) in read_counter_array(ADAPTER_DEDICATED)? {
        let Some(instance) = parse_adapter_instance(&name) else {
            continue;
        };
        samples.push(AdapterSample {
            luid: instance.luid,
            dedicated_used_bytes: to_bytes(value),
        });
    }
    Ok(samples)
}
