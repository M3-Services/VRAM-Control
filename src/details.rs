//! Text of the process details popup. Pure: it only formats data that was already collected.

use crate::app::format_age;
use crate::inventory::{GpuSummary, ProcessEntry};
use crate::model::ProcessDetails;
use crate::units::format_bytes;
use std::fmt::Write;

const UNREADABLE: &str = "(not readable)";

/// Builds the popup text. `details` is `None` when the process could not be read any more.
pub fn render_details(
    entry: &ProcessEntry,
    protected: bool,
    details: Option<&ProcessDetails>,
    gpus: &[GpuSummary],
    now_unix: u64,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Name:         {}", entry.name);
    let _ = writeln!(out, "PID:          {}", entry.pid);
    match details {
        Some(d) => {
            let _ = writeln!(
                out,
                "Path:         {}",
                d.exe.as_deref().unwrap_or(UNREADABLE)
            );
            let command = if d.cmdline.is_empty() {
                UNREADABLE
            } else {
                &d.cmdline
            };
            let _ = writeln!(out, "Command line: {command}");
            let _ = writeln!(
                out,
                "Working dir:  {}",
                d.cwd.as_deref().unwrap_or(UNREADABLE)
            );
            let parent = d.parent.as_ref().map_or_else(
                || "(none, or no longer running)".to_string(),
                |(pid, name)| format!("{name} (PID {pid})"),
            );
            let _ = writeln!(out, "Parent:       {parent}");
            let _ = writeln!(
                out,
                "User:         {}",
                d.user.as_deref().unwrap_or(UNREADABLE)
            );
            let _ = writeln!(out, "RAM:          {}", format_bytes(d.ram_bytes));
        }
        None => {
            let command = if entry.cmdline.is_empty() {
                UNREADABLE
            } else {
                &entry.cmdline
            };
            let _ = writeln!(out, "Command line: {command}");
            let _ = writeln!(
                out,
                "(The process is gone or was replaced: no more details are available.)"
            );
        }
    }
    if entry.start_time != 0 {
        let _ = writeln!(
            out,
            "Running for:  {}",
            format_age(now_unix.saturating_sub(entry.start_time))
        );
    }
    out.push('\n');
    for usage in &entry.usage {
        let gpu = gpus.iter().find(|g| g.gpu.luid == usage.luid).map_or_else(
            || usage.luid.to_string(),
            |g| format!("GPU {} {}", g.gpu.index, g.gpu.name),
        );
        let suspect = if usage.suspect {
            "  (reading above the physical VRAM)"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "VRAM on {gpu}: dedicated {}, shared {}{suspect}",
            format_bytes(usage.dedicated_bytes),
            format_bytes(usage.shared_bytes)
        );
    }
    if protected {
        out.push_str("\nProtected: this process can never be terminated by vramctl.\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::GpuUsage;
    use crate::model::{GpuInfo, Luid};

    const GIB: u64 = 1024 * 1024 * 1024;

    fn entry() -> ProcessEntry {
        ProcessEntry {
            pid: 4242,
            name: "llama-server.exe".to_string(),
            cmdline: "llama-server --model m.gguf".to_string(),
            parent_pid: Some(100),
            start_time: 1000,
            usage: vec![GpuUsage {
                luid: Luid::new(0, 1),
                dedicated_bytes: 6 * GIB,
                shared_bytes: 0,
                suspect: false,
            }],
        }
    }

    fn gpus() -> Vec<GpuSummary> {
        vec![GpuSummary {
            gpu: GpuInfo {
                index: 0,
                name: "Test GPU 0".to_string(),
                luid: Luid::new(0, 1),
                total_bytes: 24 * GIB,
            },
            used_bytes: 10 * GIB,
            attributed_bytes: 6 * GIB,
            unattributed_bytes: 4 * GIB,
        }]
    }

    fn details() -> ProcessDetails {
        ProcessDetails {
            exe: Some(r"C:\Tools\llama\llama-server.exe".to_string()),
            cmdline: "llama-server --model m.gguf".to_string(),
            cwd: Some(r"C:\Tools\llama".to_string()),
            parent: Some((100, "pwsh.exe".to_string())),
            user: Some("someone".to_string()),
            ram_bytes: 2 * GIB,
        }
    }

    #[test]
    fn shows_the_full_path_command_line_parent_user_ram_age_and_vram() {
        let text = render_details(&entry(), false, Some(&details()), &gpus(), 4600);
        assert!(text.contains(r"Path:         C:\Tools\llama\llama-server.exe"));
        assert!(text.contains("Command line: llama-server --model m.gguf"));
        assert!(text.contains(r"Working dir:  C:\Tools\llama"));
        assert!(text.contains("Parent:       pwsh.exe (PID 100)"));
        assert!(text.contains("User:         someone"));
        assert!(text.contains("RAM:          2.0 GiB"));
        assert!(text.contains("Running for:  1h 0m"));
        assert!(text.contains("VRAM on GPU 0 Test GPU 0: dedicated 6.0 GiB, shared 0 B"));
        assert!(!text.contains("Protected"));
    }

    #[test]
    fn unreadable_fields_are_labelled_and_protection_is_stated() {
        let d = ProcessDetails {
            exe: None,
            cmdline: String::new(),
            cwd: None,
            parent: None,
            user: None,
            ram_bytes: 0,
        };
        let text = render_details(&entry(), true, Some(&d), &gpus(), 4600);
        assert_eq!(text.matches(UNREADABLE).count(), 4);
        assert!(text.contains("no longer running"));
        assert!(text.contains("can never be terminated"));
    }

    #[test]
    fn a_vanished_process_still_shows_what_the_inventory_knew() {
        let text = render_details(&entry(), false, None, &gpus(), 4600);
        assert!(text.contains("llama-server --model m.gguf"));
        assert!(text.contains("gone or was replaced"));
        assert!(text.contains("dedicated 6.0 GiB"));
    }

    #[test]
    fn suspect_readings_and_unknown_start_times_are_handled() {
        let mut e = entry();
        e.start_time = 0;
        e.usage[0].suspect = true;
        let text = render_details(&e, false, Some(&details()), &gpus(), 4600);
        assert!(text.contains("reading above the physical VRAM"));
        assert!(!text.contains("Running for"));
    }
}
