//! Process details (name, command line, parent, start time) used to enrich the GPU samples.

use crate::model::ProcessMeta;
use std::collections::HashMap;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Reads every running process. Read-only.
pub fn read_process_meta() -> HashMap<u32, ProcessMeta> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::OnlyIfNotSet),
    );
    system
        .processes()
        .iter()
        .map(|(pid, process)| {
            let cmdline = process
                .cmd()
                .iter()
                .map(|part| part.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            (
                pid.as_u32(),
                ProcessMeta {
                    name: process.name().to_string_lossy().into_owned(),
                    cmdline,
                    parent_pid: process.parent().map(|p| p.as_u32()),
                    start_time: process.start_time(),
                },
            )
        })
        .collect()
}
