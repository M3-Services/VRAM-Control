//! Executes a confirmed plan. This is the only component that acts on processes, and it does so
//! through the [`ProcessControl`] trait so that tests can use a fake instead of real processes.

use crate::plan::{Plan, PlannedAction};
use crate::units::format_bytes;
use std::fmt::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The process was stopped.
    Terminated,
    /// The process had already exited.
    AlreadyGone,
    /// The PID now belongs to a different process (recycled PID): nothing was touched.
    IdentityMismatch,
    /// The caller lacks the rights to terminate the process.
    AccessDenied,
    /// The user declined the UAC prompt, so the process was left alone.
    ElevationDeclined,
    Failed(String),
}

pub trait ProcessControl {
    /// Stops the process described by `target`. `grace_ms` is the delay allowed between the
    /// graceful stop request and forced termination.
    fn stop(&self, target: &PlannedAction, grace_ms: u64) -> Outcome;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionResult {
    pub action: PlannedAction,
    pub outcome: Outcome,
}

/// Runs every action of the plan, one after the other.
pub fn execute_plan(plan: &Plan, control: &dyn ProcessControl, grace_ms: u64) -> Vec<ActionResult> {
    plan.actions
        .iter()
        .map(|action| ActionResult {
            action: action.clone(),
            outcome: control.stop(action, grace_ms),
        })
        .collect()
}

/// True when at least one action was denied or failed (useful as a process exit status).
pub fn has_failures(results: &[ActionResult]) -> bool {
    results.iter().any(|r| {
        matches!(
            r.outcome,
            Outcome::AccessDenied | Outcome::ElevationDeclined | Outcome::Failed(_)
        )
    })
}

fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Terminated => "stopped".to_string(),
        Outcome::AlreadyGone => "already gone".to_string(),
        Outcome::IdentityMismatch => {
            "skipped: the PID now belongs to a different process".to_string()
        }
        Outcome::AccessDenied => "denied: administrator rights are required".to_string(),
        Outcome::ElevationDeclined => {
            "denied: administrator rights are required and the UAC prompt was declined".to_string()
        }
        Outcome::Failed(reason) => format!("failed: {reason}"),
    }
}

/// `expected_bytes` is what the plan announced; `freed_bytes` is what was measured afterwards.
pub fn render_report(
    results: &[ActionResult],
    expected_bytes: u64,
    freed_bytes: Option<u64>,
) -> String {
    let mut out = String::new();
    for r in results {
        let _ = writeln!(
            out,
            "{:>7}  {}  {}",
            r.action.pid,
            r.action.name,
            describe(&r.outcome)
        );
    }
    let stopped = results
        .iter()
        .filter(|r| r.outcome == Outcome::Terminated)
        .count();
    let _ = writeln!(out, "\nStopped {stopped} of {} process(es).", results.len());
    if let Some(freed) = freed_bytes {
        let _ = writeln!(
            out,
            "VRAM freed: {} (expected about {}).",
            format_bytes(freed),
            format_bytes(expected_bytes)
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::ProcessAction;
    use std::cell::RefCell;
    use std::collections::HashMap;

    fn action(pid: u32, name: &str) -> PlannedAction {
        PlannedAction {
            pid,
            name: name.to_string(),
            start_time: 1,
            rule: "r".to_string(),
            action: ProcessAction::Terminate,
            tree: false,
            reclaimable_bytes: 1024 * 1024 * 1024,
        }
    }

    /// Records every call and answers with scripted outcomes (default: Terminated).
    struct FakeControl {
        calls: RefCell<Vec<(u32, u64)>>,
        scripted: HashMap<u32, Outcome>,
    }

    impl ProcessControl for FakeControl {
        fn stop(&self, target: &PlannedAction, grace_ms: u64) -> Outcome {
            self.calls.borrow_mut().push((target.pid, grace_ms));
            self.scripted
                .get(&target.pid)
                .cloned()
                .unwrap_or(Outcome::Terminated)
        }
    }

    fn fake(scripted: &[(u32, Outcome)]) -> FakeControl {
        FakeControl {
            calls: RefCell::new(Vec::new()),
            scripted: scripted.iter().cloned().collect(),
        }
    }

    fn plan(pids: &[u32]) -> Plan {
        Plan {
            actions: pids.iter().map(|p| action(*p, "x.exe")).collect(),
            ..Plan::default()
        }
    }

    #[test]
    fn runs_every_action_in_order_with_the_grace_delay() {
        let control = fake(&[]);
        let results = execute_plan(&plan(&[3, 1, 2]), &control, 2500);
        assert_eq!(
            *control.calls.borrow(),
            vec![(3, 2500), (1, 2500), (2, 2500)]
        );
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.outcome == Outcome::Terminated));
    }

    #[test]
    fn an_empty_plan_calls_nothing() {
        let control = fake(&[]);
        assert!(execute_plan(&Plan::default(), &control, 1000).is_empty());
        assert!(control.calls.borrow().is_empty());
    }

    #[test]
    fn a_failure_does_not_stop_the_remaining_actions() {
        let control = fake(&[(1, Outcome::AccessDenied)]);
        let results = execute_plan(&plan(&[1, 2]), &control, 1000);
        assert_eq!(results[0].outcome, Outcome::AccessDenied);
        assert_eq!(results[1].outcome, Outcome::Terminated);
        assert!(has_failures(&results));
    }

    #[test]
    fn a_declined_elevation_is_a_failure_and_is_explained() {
        let control = fake(&[(1, Outcome::ElevationDeclined)]);
        let results = execute_plan(&plan(&[1]), &control, 1000);
        assert!(has_failures(&results));
        assert!(render_report(&results, 0, None).contains("UAC prompt was declined"));
    }

    #[test]
    fn already_gone_and_identity_mismatch_are_not_failures() {
        let control = fake(&[(1, Outcome::AlreadyGone), (2, Outcome::IdentityMismatch)]);
        let results = execute_plan(&plan(&[1, 2]), &control, 1000);
        assert!(!has_failures(&results));
    }

    #[test]
    fn the_report_summarizes_outcomes_and_freed_memory() {
        let control = fake(&[
            (2, Outcome::AccessDenied),
            (3, Outcome::Failed("boom".to_string())),
        ]);
        let results = execute_plan(&plan(&[1, 2, 3]), &control, 1000);
        let text = render_report(
            &results,
            3 * 1024 * 1024 * 1024,
            Some(2 * 1024 * 1024 * 1024),
        );
        assert!(text.contains("stopped"));
        assert!(text.contains("denied: administrator rights are required"));
        assert!(text.contains("failed: boom"));
        assert!(text.contains("Stopped 1 of 3 process(es)."));
        assert!(text.contains("VRAM freed: 2.0 GiB (expected about 3.0 GiB)."));
        let without = render_report(&results, 0, None);
        assert!(!without.contains("VRAM freed"));
    }
}
