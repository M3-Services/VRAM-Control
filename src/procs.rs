//! Process details (name, command line, parent, start time) used to enrich the GPU samples.

use crate::model::{ProcessDetails, ProcessMeta};
use std::collections::HashMap;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind, Users};

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

/// Reads the details of one process. Returns `None` when the process is gone or when the PID now
/// belongs to a different process (its start time differs from `expected_start_time`).
pub fn read_process_details(pid: u32, expected_start_time: u64) -> Option<ProcessDetails> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_exe(UpdateKind::Always)
            .with_cwd(UpdateKind::Always)
            .with_user(UpdateKind::Always)
            .with_memory(),
    );
    let process = system.process(Pid::from_u32(pid))?;
    if process.start_time() != expected_start_time {
        return None;
    }
    let users = Users::new_with_refreshed_list();
    Some(ProcessDetails {
        exe: process.exe().map(|p| p.display().to_string()),
        cmdline: process
            .cmd()
            .iter()
            .map(|part| part.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" "),
        cwd: process.cwd().map(|p| p.display().to_string()),
        parent: process.parent().and_then(|parent| {
            system
                .process(parent)
                .map(|p| (parent.as_u32(), p.name().to_string_lossy().into_owned()))
        }),
        user: process
            .user_id()
            .and_then(|uid| users.get_user_by_id(uid))
            .map(|u| u.name().to_string()),
        ram_bytes: process.memory(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_details_of_the_current_process() {
        let pid = std::process::id();
        let start = read_process_meta()[&pid].start_time;
        let details =
            read_process_details(pid, start).expect("the current process must be readable");
        assert!(details.exe.is_some());
        assert!(!details.cmdline.is_empty());
        assert!(details.ram_bytes > 0);
    }

    #[test]
    fn a_wrong_start_time_or_a_missing_pid_gives_nothing() {
        let pid = std::process::id();
        let start = read_process_meta()[&pid].start_time;
        assert!(read_process_details(pid, start + 1).is_none());
        assert!(read_process_details(u32::MAX - 1, 0).is_none());
    }
}
