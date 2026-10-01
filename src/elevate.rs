//! Administrator elevation for processes the current user cannot terminate (services, processes of
//! other sessions...).
//!
//! The tool itself never runs elevated. When some terminations are denied, it relaunches itself
//! through the Windows "runas" verb as a short-lived helper (`vramctl elevated-stop`): the user
//! sees ONE UAC prompt for the whole batch and only has to accept it. The helper does nothing but
//! terminate the listed processes. It re-checks every target on its own (protection list, PID and
//! start time) and reports the outcome of each one through a small text file.

use crate::executor::{ActionResult, Outcome, ProcessControl, execute_plan};
use crate::plan::{Plan, PlannedAction, ProcessAction};
use crate::protect::Protection;
use crate::winproc::{Handle, WindowsControl, wait_for_exit};
use anyhow::{Context, Result, bail};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::PCWSTR;

/// Name of the hidden subcommand run by the elevated helper.
pub const HELPER_COMMAND: &str = "elevated-stop";

/// Extra time granted to the helper on top of the grace delays (UAC prompt, process start-up).
const HELPER_SLACK_MS: u64 = 120_000;

/// One process to stop inside the elevated helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobTarget {
    pub pid: u32,
    pub start_time: u64,
    pub action: ProcessAction,
    pub tree: bool,
}

impl JobTarget {
    fn to_arg(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.pid,
            self.start_time,
            self.action,
            u8::from(self.tree)
        )
    }

    pub fn parse(text: &str) -> Result<JobTarget> {
        let parts: Vec<&str> = text.split(':').collect();
        let [pid, start_time, action, tree] = parts[..] else {
            bail!("invalid target '{text}' (expected pid:start_time:action:tree)");
        };
        let action = match action {
            "terminate" => ProcessAction::Terminate,
            "kill" => ProcessAction::Kill,
            other => bail!("invalid action '{other}' in target '{text}'"),
        };
        let tree = match tree {
            "0" => false,
            "1" => true,
            other => bail!("invalid tree flag '{other}' in target '{text}'"),
        };
        Ok(JobTarget {
            pid: pid
                .parse()
                .with_context(|| format!("invalid PID in '{text}'"))?,
            start_time: start_time
                .parse()
                .with_context(|| format!("invalid start time in '{text}'"))?,
            action,
            tree,
        })
    }
}

/// What the elevated helper has to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElevatedJob {
    pub targets: Vec<JobTarget>,
    /// Extra protected process names (from the `[protect]` section of the configuration).
    pub protect: Vec<String>,
    pub grace_ms: u64,
}

impl ElevatedJob {
    pub fn from_parts(
        grace_ms: u64,
        protect: Vec<String>,
        targets: &[String],
    ) -> Result<ElevatedJob> {
        Ok(ElevatedJob {
            targets: targets
                .iter()
                .map(|t| JobTarget::parse(t))
                .collect::<Result<_>>()?,
            protect,
            grace_ms,
        })
    }
}

/// Command-line arguments that make `vramctl` run the helper for `job`, writing results to `out`.
pub fn job_args(job: &ElevatedJob, out: &Path) -> Vec<String> {
    let mut args = vec![
        HELPER_COMMAND.to_string(),
        "--out".to_string(),
        out.display().to_string(),
        "--grace-ms".to_string(),
        job.grace_ms.to_string(),
    ];
    for name in &job.protect {
        args.push("--protect".to_string());
        args.push(name.clone());
    }
    for target in &job.targets {
        args.push("--target".to_string());
        args.push(target.to_arg());
    }
    args
}

/// Quotes one argument following the Windows command-line rules.
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

pub fn command_line(args: &[String]) -> String {
    args.iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn outcome_to_wire(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Terminated => "terminated".to_string(),
        Outcome::AlreadyGone => "already-gone".to_string(),
        Outcome::IdentityMismatch => "identity-mismatch".to_string(),
        Outcome::AccessDenied => "access-denied".to_string(),
        Outcome::ElevationDeclined => "elevation-declined".to_string(),
        Outcome::Failed(reason) => format!("failed:{}", reason.replace(['\n', '\r', '\t'], " ")),
    }
}

pub fn outcome_from_wire(text: &str) -> Option<Outcome> {
    Some(match text {
        "terminated" => Outcome::Terminated,
        "already-gone" => Outcome::AlreadyGone,
        "identity-mismatch" => Outcome::IdentityMismatch,
        "access-denied" => Outcome::AccessDenied,
        "elevation-declined" => Outcome::ElevationDeclined,
        other => Outcome::Failed(other.strip_prefix("failed:")?.to_string()),
    })
}

/// One `pid<TAB>outcome` line per target.
pub fn results_to_text(results: &[(u32, Outcome)]) -> String {
    results
        .iter()
        .map(|(pid, outcome)| format!("{pid}\t{}\n", outcome_to_wire(outcome)))
        .collect()
}

/// Parses the helper's result file, skipping any line that is not understood.
pub fn results_from_text(text: &str) -> Vec<(u32, Outcome)> {
    text.lines()
        .filter_map(|line| {
            let (pid, outcome) = line.split_once('\t')?;
            Some((pid.parse().ok()?, outcome_from_wire(outcome)?))
        })
        .collect()
}

/// The helper's logic. `name_of` returns the live name of a process (None when it is gone).
///
/// Every target is re-checked here, whatever the caller said: a protected process is refused, and
/// the control layer re-verifies PID and start time before terminating.
pub fn execute_job(
    job: &ElevatedJob,
    control: &dyn ProcessControl,
    name_of: &dyn Fn(u32) -> Option<String>,
    self_pid: u32,
) -> Vec<(u32, Outcome)> {
    let protection = Protection::new(&job.protect, self_pid);
    job.targets
        .iter()
        .map(|target| {
            let outcome = match name_of(target.pid) {
                None => Outcome::AlreadyGone,
                Some(name) if protection.is_protected(target.pid, &name) => {
                    Outcome::Failed("refused: protected process".to_string())
                }
                Some(name) => {
                    let planned = PlannedAction {
                        pid: target.pid,
                        name,
                        start_time: target.start_time,
                        rule: "elevated".to_string(),
                        action: target.action,
                        tree: target.tree,
                        reclaimable_bytes: 0,
                    };
                    match control.stop(&planned, job.grace_ms) {
                        Outcome::AccessDenied => Outcome::Failed(
                            "access denied even with administrator rights (protected process)"
                                .to_string(),
                        ),
                        other => other,
                    }
                }
            };
            (target.pid, outcome)
        })
        .collect()
}

fn live_process_name(pid: u32) -> Option<String> {
    let mut system = System::new();
    let pid = Pid::from_u32(pid);
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    system
        .process(pid)
        .map(|p| p.name().to_string_lossy().into_owned())
}

/// Entry point of the elevated helper: runs the job for real and writes the results file.
pub fn run_elevated_job(job: &ElevatedJob, out: &Path) -> Result<()> {
    let results = execute_job(job, &WindowsControl, &live_process_name, std::process::id());
    std::fs::write(out, results_to_text(&results))
        .with_context(|| format!("cannot write the results to {}", out.display()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElevationOutcome {
    /// The helper ran; one outcome per target.
    Done(Vec<(u32, Outcome)>),
    /// The user declined the UAC prompt.
    Declined,
    /// The helper could not be started or did not report back.
    Unavailable(String),
}

pub trait Elevator {
    fn run(&self, targets: &[PlannedAction], protect: &[String], grace_ms: u64)
    -> ElevationOutcome;
}

/// Runs the plan without elevation first, then hands every denied action to the elevator in one
/// batch. With `elevator = None`, denied actions simply stay denied.
pub fn execute_with_elevation(
    plan: &Plan,
    control: &dyn ProcessControl,
    elevator: Option<&dyn Elevator>,
    protect: &[String],
    grace_ms: u64,
) -> Vec<ActionResult> {
    let mut results = execute_plan(plan, control, grace_ms);
    let Some(elevator) = elevator else {
        return results;
    };
    let denied: Vec<PlannedAction> = results
        .iter()
        .filter(|r| r.outcome == Outcome::AccessDenied)
        .map(|r| r.action.clone())
        .collect();
    if denied.is_empty() {
        return results;
    }
    let answer = elevator.run(&denied, protect, grace_ms);
    for result in results
        .iter_mut()
        .filter(|r| r.outcome == Outcome::AccessDenied)
    {
        result.outcome = match &answer {
            ElevationOutcome::Done(list) => list
                .iter()
                .find(|(pid, _)| *pid == result.action.pid)
                .map_or_else(
                    || Outcome::Failed("no result from the elevated helper".to_string()),
                    |(_, outcome)| outcome.clone(),
                ),
            ElevationOutcome::Declined => Outcome::ElevationDeclined,
            ElevationOutcome::Unavailable(reason) => {
                Outcome::Failed(format!("elevation unavailable: {reason}"))
            }
        };
    }
    results
}

/// True when the current process already runs with administrator rights.
pub fn is_elevated() -> bool {
    // SAFETY: standard token query on the current process; the token handle is closed below.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(std::ptr::from_mut(&mut elevation).cast::<c_void>()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn results_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("vramctl-{}-{nanos}.txt", std::process::id()))
}

/// Starts the helper through the "runas" verb (UAC prompt) and waits for it.
pub struct WindowsElevator;

impl Elevator for WindowsElevator {
    fn run(
        &self,
        targets: &[PlannedAction],
        protect: &[String],
        grace_ms: u64,
    ) -> ElevationOutcome {
        let exe = match std::env::current_exe() {
            Ok(path) => path,
            Err(e) => return ElevationOutcome::Unavailable(e.to_string()),
        };
        let job = ElevatedJob {
            targets: targets
                .iter()
                .map(|t| JobTarget {
                    pid: t.pid,
                    start_time: t.start_time,
                    action: t.action,
                    tree: t.tree,
                })
                .collect(),
            protect: protect.to_vec(),
            grace_ms,
        };
        let out = results_path();
        let parameters = wide(&command_line(&job_args(&job, &out)));
        let verb = wide("runas");
        let file = wide(&exe.display().to_string());

        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(parameters.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        // SAFETY: `info` and the wide strings it points to outlive the call.
        if let Err(e) = unsafe { ShellExecuteExW(&mut info) } {
            return if e.code() == ERROR_CANCELLED.to_hresult() {
                ElevationOutcome::Declined
            } else {
                ElevationOutcome::Unavailable(e.message())
            };
        }
        if info.hProcess.is_invalid() {
            return ElevationOutcome::Unavailable("no handle for the elevated helper".to_string());
        }
        let helper = Handle::new(info.hProcess);
        let timeout = u64::try_from(job.targets.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(grace_ms)
            .saturating_add(HELPER_SLACK_MS);
        let finished = wait_for_exit(&helper, u32::try_from(timeout).unwrap_or(u32::MAX));
        drop(helper);

        let text = std::fs::read_to_string(&out);
        let _ = std::fs::remove_file(&out);
        match (finished, text) {
            (_, Ok(text)) => ElevationOutcome::Done(results_from_text(&text)),
            (false, Err(_)) => ElevationOutcome::Unavailable(
                "the elevated helper did not finish in time".to_string(),
            ),
            (true, Err(e)) => {
                ElevationOutcome::Unavailable(format!("the elevated helper left no result: {e}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procs::read_process_meta;
    use std::cell::RefCell;
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    fn target(pid: u32) -> JobTarget {
        JobTarget {
            pid,
            start_time: 1_789_000_000 + u64::from(pid),
            action: ProcessAction::Terminate,
            tree: false,
        }
    }

    fn planned(pid: u32) -> PlannedAction {
        PlannedAction {
            pid,
            name: format!("p{pid}.exe"),
            start_time: 100 + u64::from(pid),
            rule: "r".to_string(),
            action: ProcessAction::Kill,
            tree: true,
            reclaimable_bytes: 0,
        }
    }

    #[test]
    fn targets_survive_a_round_trip_through_the_command_line() {
        let t = JobTarget {
            pid: 4242,
            start_time: 1_789_477_252,
            action: ProcessAction::Kill,
            tree: true,
        };
        assert_eq!(t.to_arg(), "4242:1789477252:kill:1");
        assert_eq!(JobTarget::parse("4242:1789477252:kill:1").unwrap(), t);
        assert_eq!(
            JobTarget::parse("7:0:terminate:0").unwrap(),
            JobTarget {
                pid: 7,
                start_time: 0,
                action: ProcessAction::Terminate,
                tree: false
            }
        );
    }

    #[test]
    fn malformed_targets_are_rejected() {
        for bad in [
            "",
            "1:2:kill",
            "x:2:kill:0",
            "1:y:kill:0",
            "1:2:nuke:0",
            "1:2:kill:2",
            "1:2:kill:0:9",
        ] {
            assert!(JobTarget::parse(bad).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn the_helper_arguments_carry_everything_the_helper_needs() {
        let job = ElevatedJob {
            targets: vec![target(10), target(11)],
            protect: vec!["my app.exe".to_string()],
            grace_ms: 3000,
        };
        let args = job_args(&job, Path::new(r"C:\Temp\out file.txt"));
        assert_eq!(args[0], HELPER_COMMAND);
        assert_eq!(
            args[1..5],
            ["--out", r"C:\Temp\out file.txt", "--grace-ms", "3000"]
        );
        assert_eq!(args[5..7], ["--protect", "my app.exe"]);
        assert_eq!(args.iter().filter(|a| *a == "--target").count(), 2);
        let line = command_line(&args);
        assert!(line.starts_with("elevated-stop --out \"C:\\Temp\\out file.txt\""));
        assert!(line.contains("--protect \"my app.exe\""));
    }

    #[test]
    fn arguments_are_quoted_like_windows_expects() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(
            quote_arg(r"C:\dir with space\"),
            "\"C:\\dir with space\\\\\""
        );
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_arg(r"C:\no\spaces"), r"C:\no\spaces");
    }

    #[test]
    fn outcomes_survive_the_result_file() {
        let outcomes = vec![
            (1, Outcome::Terminated),
            (2, Outcome::AlreadyGone),
            (3, Outcome::IdentityMismatch),
            (4, Outcome::AccessDenied),
            (5, Outcome::ElevationDeclined),
            (6, Outcome::Failed("boom\nwith\ttabs".to_string())),
        ];
        let parsed = results_from_text(&results_to_text(&outcomes));
        assert_eq!(parsed.len(), 6);
        assert_eq!(parsed[..5], outcomes[..5]);
        assert_eq!(
            parsed[5],
            (6, Outcome::Failed("boom with tabs".to_string()))
        );
    }

    #[test]
    fn unreadable_result_lines_are_skipped() {
        let parsed = results_from_text("garbage\n12\tterminated\nx\tterminated\n13\twhat\n");
        assert_eq!(parsed, vec![(12, Outcome::Terminated)]);
    }

    struct FakeControl {
        calls: RefCell<Vec<u32>>,
        answers: Vec<(u32, Outcome)>,
    }

    impl ProcessControl for FakeControl {
        fn stop(&self, target: &PlannedAction, _grace_ms: u64) -> Outcome {
            self.calls.borrow_mut().push(target.pid);
            self.answers
                .iter()
                .find(|(pid, _)| *pid == target.pid)
                .map_or(Outcome::Terminated, |(_, o)| o.clone())
        }
    }

    fn fake(answers: &[(u32, Outcome)]) -> FakeControl {
        FakeControl {
            calls: RefCell::new(Vec::new()),
            answers: answers.to_vec(),
        }
    }

    fn name_of(pid: u32) -> Option<String> {
        match pid {
            999 => None,
            500 => Some("dwm.exe".to_string()),
            _ => Some(format!("p{pid}.exe")),
        }
    }

    #[test]
    fn the_helper_refuses_protected_processes_and_reports_vanished_ones() {
        let job = ElevatedJob {
            targets: vec![target(10), target(500), target(999), target(11), target(12)],
            protect: vec!["p11.exe".to_string()],
            grace_ms: 1000,
        };
        let control = fake(&[(12, Outcome::AccessDenied)]);
        let results = execute_job(&job, &control, &name_of, 4321);
        assert_eq!(results[0], (10, Outcome::Terminated));
        assert_eq!(
            results[1],
            (
                500,
                Outcome::Failed("refused: protected process".to_string())
            )
        );
        assert_eq!(results[2], (999, Outcome::AlreadyGone));
        assert_eq!(
            results[3],
            (
                11,
                Outcome::Failed("refused: protected process".to_string())
            )
        );
        assert!(
            matches!(&results[4], (12, Outcome::Failed(m)) if m.contains("even with administrator rights"))
        );
        // Protected and vanished targets never reach the control layer.
        assert_eq!(*control.calls.borrow(), vec![10, 12]);
    }

    #[test]
    fn the_helper_never_terminates_itself() {
        let job = ElevatedJob {
            targets: vec![target(4321)],
            protect: vec![],
            grace_ms: 1000,
        };
        let control = fake(&[]);
        let results = execute_job(&job, &control, &name_of, 4321);
        assert!(matches!(&results[0].1, Outcome::Failed(_)));
        assert!(control.calls.borrow().is_empty());
    }

    /// (target PIDs, protection list, grace delay) of one elevator call.
    type Call = (Vec<u32>, Vec<String>, u64);

    struct FakeElevator {
        answer: ElevationOutcome,
        calls: RefCell<Vec<Call>>,
    }

    impl Elevator for FakeElevator {
        fn run(
            &self,
            targets: &[PlannedAction],
            protect: &[String],
            grace_ms: u64,
        ) -> ElevationOutcome {
            self.calls.borrow_mut().push((
                targets.iter().map(|t| t.pid).collect(),
                protect.to_vec(),
                grace_ms,
            ));
            self.answer.clone()
        }
    }

    fn elevator(answer: ElevationOutcome) -> FakeElevator {
        FakeElevator {
            answer,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn plan(pids: &[u32]) -> Plan {
        Plan {
            actions: pids.iter().map(|p| planned(*p)).collect(),
            ..Plan::default()
        }
    }

    #[test]
    fn denied_actions_are_sent_to_the_elevator_in_one_batch_and_merged() {
        let control = fake(&[(2, Outcome::AccessDenied), (3, Outcome::AccessDenied)]);
        let elev = elevator(ElevationOutcome::Done(vec![
            (2, Outcome::Terminated),
            (3, Outcome::Failed("x".to_string())),
        ]));
        let protect = vec!["keep.exe".to_string()];
        let results =
            execute_with_elevation(&plan(&[1, 2, 3]), &control, Some(&elev), &protect, 2500);
        let outcomes: Vec<&Outcome> = results.iter().map(|r| &r.outcome).collect();
        assert_eq!(outcomes[0], &Outcome::Terminated);
        assert_eq!(outcomes[1], &Outcome::Terminated);
        assert_eq!(outcomes[2], &Outcome::Failed("x".to_string()));
        // Exactly one call, with only the denied PIDs, the protection list and the grace delay.
        assert_eq!(*elev.calls.borrow(), vec![(vec![2, 3], protect, 2500)]);
    }

    #[test]
    fn nothing_is_elevated_when_nothing_was_denied() {
        let control = fake(&[]);
        let elev = elevator(ElevationOutcome::Declined);
        let results = execute_with_elevation(&plan(&[1, 2]), &control, Some(&elev), &[], 1000);
        assert!(results.iter().all(|r| r.outcome == Outcome::Terminated));
        assert!(elev.calls.borrow().is_empty());
    }

    #[test]
    fn without_an_elevator_denied_actions_stay_denied() {
        let control = fake(&[(1, Outcome::AccessDenied)]);
        let results = execute_with_elevation(&plan(&[1]), &control, None, &[], 1000);
        assert_eq!(results[0].outcome, Outcome::AccessDenied);
    }

    #[test]
    fn a_declined_prompt_and_an_unavailable_helper_are_reported() {
        let control = fake(&[(1, Outcome::AccessDenied)]);
        let declined = elevator(ElevationOutcome::Declined);
        let r = execute_with_elevation(&plan(&[1]), &control, Some(&declined), &[], 1000);
        assert_eq!(r[0].outcome, Outcome::ElevationDeclined);

        let broken = elevator(ElevationOutcome::Unavailable("no exe".to_string()));
        let r = execute_with_elevation(&plan(&[1]), &control, Some(&broken), &[], 1000);
        assert_eq!(
            r[0].outcome,
            Outcome::Failed("elevation unavailable: no exe".to_string())
        );

        let silent = elevator(ElevationOutcome::Done(vec![]));
        let r = execute_with_elevation(&plan(&[1]), &control, Some(&silent), &[], 1000);
        assert_eq!(
            r[0].outcome,
            Outcome::Failed("no result from the elevated helper".to_string())
        );
    }

    /// A harmless child started by the test itself, killed on drop as a safety net.
    struct TestChild(Child);

    impl Drop for TestChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn the_real_helper_logic_terminates_a_child_and_writes_the_result_file() {
        let mut child = TestChild(
            Command::new("ping")
                .args(["-n", "300", "127.0.0.1"])
                .stdout(Stdio::null())
                .spawn()
                .expect("failed to start ping"),
        );
        let pid = child.0.id();
        let mut start_time = None;
        for _ in 0..50 {
            if let Some(meta) = read_process_meta().get(&pid) {
                start_time = Some(meta.start_time);
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let job = ElevatedJob {
            targets: vec![JobTarget {
                pid,
                start_time: start_time.expect("the child never appeared"),
                action: ProcessAction::Kill,
                tree: false,
            }],
            protect: vec![],
            grace_ms: 500,
        };
        let out = results_path();
        run_elevated_job(&job, &out).unwrap();
        let results = results_from_text(&std::fs::read_to_string(&out).unwrap());
        let _ = std::fs::remove_file(&out);
        assert_eq!(results, vec![(pid, Outcome::Terminated)]);
        assert!(child.0.wait().is_ok());
    }

    #[test]
    fn the_elevation_state_can_be_read() {
        // Either answer is valid; the call itself must work and agree with itself.
        assert_eq!(is_elevated(), is_elevated());
    }
}
