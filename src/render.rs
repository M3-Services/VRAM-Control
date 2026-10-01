//! Plain-text rendering of the inventory for the `list` and `gpus` commands.

use crate::inventory::{GpuSummary, Inventory};
use crate::model::GpuInfo;
use crate::units::format_bytes;
use std::collections::HashMap;
use std::fmt::Write;

const MAX_NAME_WIDTH: usize = 32;

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let kept: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}~")
    }
}

pub fn render_gpus(gpus: &[GpuInfo]) -> String {
    let mut out = String::new();
    for gpu in gpus {
        let _ = writeln!(
            out,
            "GPU {}: {} ({}, luid {})",
            gpu.index,
            gpu.name,
            format_bytes(gpu.total_bytes),
            gpu.luid
        );
    }
    out
}

fn render_summary(summary: &GpuSummary) -> String {
    format!(
        "GPU {}: {}  used {} / {}  (attributed {}, unattributed {})",
        summary.gpu.index,
        summary.gpu.name,
        format_bytes(summary.used_bytes),
        format_bytes(summary.gpu.total_bytes),
        format_bytes(summary.attributed_bytes),
        format_bytes(summary.unattributed_bytes),
    )
}

/// Renders the GPU summaries and a process table.
/// Processes using less than `min_bytes` of dedicated memory are left out.
/// The GPU column only appears when more than one GPU is present.
pub fn render_table(inv: &Inventory, min_bytes: u64) -> String {
    let mut out = String::new();
    for summary in &inv.gpus {
        let _ = writeln!(out, "{}", render_summary(summary));
    }
    out.push('\n');

    let index_by_luid: HashMap<_, _> = inv.gpus.iter().map(|s| (s.gpu.luid, s.gpu.index)).collect();
    let multi_gpu = inv.gpus.len() > 1;

    struct Row {
        pid: String,
        name: String,
        dedicated: String,
        shared: String,
        gpus: String,
    }

    let mut any_suspect = false;
    let rows: Vec<Row> = inv
        .processes
        .iter()
        .filter(|p| p.dedicated_bytes() >= min_bytes)
        .map(|p| {
            let mut gpu_ids: Vec<String> = p
                .usage
                .iter()
                .map(|u| {
                    index_by_luid
                        .get(&u.luid)
                        .map_or_else(|| "?".to_string(), |i| i.to_string())
                })
                .collect();
            gpu_ids.dedup();
            let suspect = p.is_suspect();
            any_suspect |= suspect;
            Row {
                pid: p.pid.to_string(),
                name: truncate(&p.name, MAX_NAME_WIDTH),
                dedicated: format!(
                    "{}{}",
                    format_bytes(p.dedicated_bytes()),
                    if suspect { " ?" } else { "" }
                ),
                shared: format_bytes(p.shared_bytes()),
                gpus: gpu_ids.join(","),
            }
        })
        .collect();

    let width = |header: &str, f: fn(&Row) -> &str| {
        rows.iter()
            .map(|r| f(r).chars().count())
            .chain([header.len()])
            .max()
            .unwrap_or(0)
    };
    let w_pid = width("PID", |r| &r.pid);
    let w_name = width("NAME", |r| &r.name);
    let w_ded = width("DEDICATED", |r| &r.dedicated);
    let w_sha = width("SHARED", |r| &r.shared);

    let mut header = format!(
        "{:>w_pid$}  {:<w_name$}  {:>w_ded$}  {:>w_sha$}",
        "PID", "NAME", "DEDICATED", "SHARED"
    );
    if multi_gpu {
        header.push_str("  GPU");
    }
    let _ = writeln!(out, "{header}");
    for r in &rows {
        let mut line = format!(
            "{:>w_pid$}  {:<w_name$}  {:>w_ded$}  {:>w_sha$}",
            r.pid, r.name, r.dedicated, r.shared
        );
        if multi_gpu {
            let _ = write!(line, "  {}", r.gpus);
        }
        let _ = writeln!(out, "{line}");
    }
    if any_suspect {
        let _ = writeln!(
            out,
            "\n? = reading above the physical VRAM; shown but excluded from the attributed total."
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::build_inventory;
    use crate::model::{AdapterSample, Luid, ProcessMeta, ProcessSample};

    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    fn gpu(index: usize, low: u32, total_gib: u64) -> GpuInfo {
        GpuInfo {
            index,
            name: format!("Test GPU {index}"),
            luid: Luid::new(0, low),
            total_bytes: total_gib * GIB,
        }
    }

    fn sample(pid: u32, low: u32, dedicated: u64) -> ProcessSample {
        ProcessSample {
            pid,
            luid: Luid::new(0, low),
            dedicated_bytes: dedicated,
            shared_bytes: 0,
        }
    }

    fn metas(entries: &[(u32, &str)]) -> HashMap<u32, ProcessMeta> {
        entries
            .iter()
            .map(|(pid, name)| {
                (
                    *pid,
                    ProcessMeta {
                        name: name.to_string(),
                        cmdline: String::new(),
                        parent_pid: None,
                        start_time: 0,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn single_gpu_table_has_no_gpu_column_and_shows_the_summary() {
        let gpus = [gpu(0, 1, 24)];
        let adapters = [AdapterSample {
            luid: Luid::new(0, 1),
            dedicated_used_bytes: 10 * GIB,
        }];
        let inv = build_inventory(
            &gpus,
            &adapters,
            &[sample(10, 1, 6 * GIB)],
            &metas(&[(10, "llama-server.exe")]),
        );
        let text = render_table(&inv, 0);
        assert!(text.contains("GPU 0: Test GPU 0  used 10.0 GiB / 24.0 GiB"));
        assert!(text.contains("unattributed 4.0 GiB"));
        assert!(text.contains("llama-server.exe"));
        assert!(text.contains("6.0 GiB"));
        assert!(!text.contains("  GPU\n"));
    }

    #[test]
    fn multi_gpu_table_has_a_gpu_column() {
        let gpus = [gpu(0, 1, 24), gpu(1, 2, 12)];
        let inv = build_inventory(
            &gpus,
            &[],
            &[sample(10, 1, GIB), sample(10, 2, GIB)],
            &metas(&[(10, "a.exe")]),
        );
        let text = render_table(&inv, 0);
        assert!(text.lines().any(|l| l.ends_with("  GPU")));
        assert!(
            text.lines()
                .any(|l| l.contains("a.exe") && l.ends_with("  0,1"))
        );
    }

    #[test]
    fn small_consumers_are_filtered_out() {
        let gpus = [gpu(0, 1, 24)];
        let inv = build_inventory(
            &gpus,
            &[],
            &[sample(10, 1, GIB), sample(11, 1, 10 * MIB)],
            &metas(&[(10, "big.exe"), (11, "tiny.exe")]),
        );
        let text = render_table(&inv, 100 * MIB);
        assert!(text.contains("big.exe"));
        assert!(!text.contains("tiny.exe"));
    }

    #[test]
    fn suspect_readings_are_marked_and_explained() {
        let gpus = [gpu(0, 1, 24)];
        let inv = build_inventory(
            &gpus,
            &[],
            &[sample(10, 1, 55 * GIB)],
            &metas(&[(10, "dwm.exe")]),
        );
        let text = render_table(&inv, 0);
        assert!(text.contains("55.0 GiB ?"));
        assert!(text.contains("excluded from the attributed total"));
    }

    #[test]
    fn long_names_are_truncated() {
        let long = "a".repeat(60);
        let gpus = [gpu(0, 1, 24)];
        let inv = build_inventory(&gpus, &[], &[sample(10, 1, GIB)], &metas(&[(10, &long)]));
        let text = render_table(&inv, 0);
        assert!(text.contains(&format!("{}~", "a".repeat(31))));
        assert!(!text.contains(&long));
    }

    #[test]
    fn lists_gpus() {
        let text = render_gpus(&[gpu(0, 1, 24)]);
        assert_eq!(
            text,
            "GPU 0: Test GPU 0 (24.0 GiB, luid 0x00000000_0x00000001)\n"
        );
    }
}
