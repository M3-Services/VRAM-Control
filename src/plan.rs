//! The cleanup plan: what would be terminated, and why. Building a plan never acts on anything.

use crate::units::format_bytes;
use std::fmt;
use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessAction {
    /// Graceful stop first, forced termination after the grace delay.
    Terminate,
    /// Forced termination right away.
    Kill,
}

impl fmt::Display for ProcessAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ProcessAction::Terminate => "terminate",
            ProcessAction::Kill => "kill",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedAction {
    pub pid: u32,
    pub name: String,
    /// Start time recorded at planning time, used to make sure a recycled PID is never hit.
    pub start_time: u64,
    pub rule: String,
    pub action: ProcessAction,
    /// Also terminate the descendants of the process.
    pub tree: bool,
    /// Dedicated VRAM the process holds (readings flagged as suspect count as 0).
    pub reclaimable_bytes: u64,
}

/// A process that a rule selected but that the protection list refuses to touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedHit {
    pub pid: u32,
    pub name: String,
    pub rule: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub actions: Vec<PlannedAction>,
    pub protected: Vec<ProtectedHit>,
    /// Remarks for the user (for example rules that are ignored).
    pub notes: Vec<String>,
}

impl Plan {
    pub fn total_reclaimable(&self) -> u64 {
        self.actions.iter().map(|a| a.reclaimable_bytes).sum()
    }
}

pub fn render_plan(plan: &Plan) -> String {
    let mut out = String::new();
    if plan.actions.is_empty() {
        out.push_str("Nothing to do: no running process matches the rules.\n");
    } else {
        let _ = writeln!(
            out,
            "Cleanup plan: {} process(es), about {} of VRAM to free.\n",
            plan.actions.len(),
            format_bytes(plan.total_reclaimable())
        );
        let w_name = plan
            .actions
            .iter()
            .map(|a| a.name.chars().count())
            .chain([4])
            .max()
            .unwrap_or(4);
        let _ = writeln!(
            out,
            "{:>7}  {:<w_name$}  {:<9}  {:>10}  RULE",
            "PID", "NAME", "ACTION", "VRAM"
        );
        for a in &plan.actions {
            let action = if a.tree {
                format!("{}+tree", a.action)
            } else {
                a.action.to_string()
            };
            let _ = writeln!(
                out,
                "{:>7}  {:<w_name$}  {:<9}  {:>10}  {}",
                a.pid,
                a.name,
                action,
                format_bytes(a.reclaimable_bytes),
                a.rule
            );
        }
    }
    for hit in &plan.protected {
        let _ = writeln!(
            out,
            "Refused (protected): {} (PID {}) selected by rule '{}'",
            hit.name, hit.pid, hit.rule
        );
    }
    for note in &plan.notes {
        let _ = writeln!(out, "Note: {note}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(pid: u32, name: &str, bytes: u64, tree: bool) -> PlannedAction {
        PlannedAction {
            pid,
            name: name.to_string(),
            start_time: 1,
            rule: "my rule".to_string(),
            action: ProcessAction::Terminate,
            tree,
            reclaimable_bytes: bytes,
        }
    }

    #[test]
    fn an_empty_plan_says_there_is_nothing_to_do() {
        assert!(render_plan(&Plan::default()).contains("Nothing to do"));
    }

    #[test]
    fn renders_actions_and_the_total() {
        let plan = Plan {
            actions: vec![
                action(10, "llama-server.exe", 6 * 1024 * 1024 * 1024, false),
                action(11, "python.exe", 2 * 1024 * 1024 * 1024, true),
            ],
            ..Plan::default()
        };
        assert_eq!(plan.total_reclaimable(), 8 * 1024 * 1024 * 1024);
        let text = render_plan(&plan);
        assert!(text.contains("2 process(es), about 8.0 GiB"));
        assert!(text.contains("llama-server.exe"));
        assert!(text.contains("terminate+tree"));
        assert!(text.contains("my rule"));
    }

    #[test]
    fn renders_protected_hits_and_notes() {
        let plan = Plan {
            protected: vec![ProtectedHit {
                pid: 5,
                name: "dwm.exe".to_string(),
                rule: "r".to_string(),
            }],
            notes: vec!["something".to_string()],
            ..Plan::default()
        };
        let text = render_plan(&plan);
        assert!(text.contains("Refused (protected): dwm.exe (PID 5) selected by rule 'r'"));
        assert!(text.contains("Note: something"));
    }
}
