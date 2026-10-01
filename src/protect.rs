//! Processes that must never be terminated, whatever the rules say.

use std::collections::HashSet;

/// Names (lowercase) that are always protected. This floor cannot be reduced from the config file.
pub const BUILTIN_PROTECTED: &[&str] = &[
    "system",
    "registry",
    "memory compression",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "svchost.exe",
    "dwm.exe",
    "explorer.exe",
];

/// PIDs at or below this value belong to the kernel ("System Idle Process", "System").
const MAX_KERNEL_PID: u32 = 4;

#[derive(Debug, Clone)]
pub struct Protection {
    names: HashSet<String>,
    self_pid: u32,
}

impl Protection {
    /// `extra` comes from the `[protect]` section; `self_pid` is the PID of VRAM-Control itself.
    pub fn new(extra: &[String], self_pid: u32) -> Self {
        let names = BUILTIN_PROTECTED
            .iter()
            .map(|n| n.to_string())
            .chain(extra.iter().map(|n| n.to_lowercase()))
            .collect();
        Self { names, self_pid }
    }

    pub fn is_protected(&self, pid: u32, name: &str) -> bool {
        pid <= MAX_KERNEL_PID || pid == self.self_pid || self.names.contains(&name.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_names_are_protected_case_insensitively() {
        let p = Protection::new(&[], 9999);
        assert!(p.is_protected(500, "dwm.exe"));
        assert!(p.is_protected(500, "DWM.EXE"));
        assert!(p.is_protected(500, "Explorer.exe"));
        assert!(p.is_protected(500, "csrss.exe"));
    }

    #[test]
    fn configured_names_are_added() {
        let p = Protection::new(&["Obsidian.exe".to_string()], 9999);
        assert!(p.is_protected(500, "obsidian.exe"));
        assert!(!p.is_protected(500, "llama-server.exe"));
    }

    #[test]
    fn kernel_pids_and_the_tool_itself_are_protected() {
        let p = Protection::new(&[], 4242);
        assert!(p.is_protected(0, "anything"));
        assert!(p.is_protected(4, "anything"));
        assert!(p.is_protected(4242, "vramctl.exe"));
        assert!(!p.is_protected(4243, "vramctl.exe"));
    }
}
