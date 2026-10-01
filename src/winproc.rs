//! Terminates Windows processes. OS layer behind [`ProcessControl`].
//!
//! Safety nets: the target is re-identified by PID and start time right before acting (a recycled
//! PID is never hit), and nothing here is reachable without a confirmed plan.

use crate::executor::{Outcome, ProcessControl};
use crate::plan::{PlannedAction, ProcessAction};
use std::collections::{HashMap, HashSet};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE, HWND, LPARAM, WAIT_OBJECT_0,
    WPARAM,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
};
use windows::core::BOOL;

/// How long to wait for a forcibly terminated process to actually disappear.
const FORCE_WAIT_MS: u32 = 5000;

pub struct WindowsControl;

/// Owns a process handle and closes it when dropped.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by OpenProcess and is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn open(
    pid: u32,
    access: windows::Win32::System::Threading::PROCESS_ACCESS_RIGHTS,
) -> Result<Handle, Outcome> {
    // SAFETY: plain Win32 call with value arguments.
    match unsafe { OpenProcess(access, false, pid) } {
        Ok(handle) => Ok(Handle(handle)),
        Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => Err(Outcome::AccessDenied),
        Err(e) if e.code() == ERROR_INVALID_PARAMETER.to_hresult() => Err(Outcome::AlreadyGone),
        Err(e) => Err(Outcome::Failed(e.message())),
    }
}

fn wait_for_exit(handle: &Handle, timeout_ms: u32) -> bool {
    // SAFETY: the handle is valid for the lifetime of `handle`.
    unsafe { WaitForSingleObject(handle.0, timeout_ms) == WAIT_OBJECT_0 }
}

/// Terminates the process immediately and waits until it is gone.
fn force_terminate(pid: u32) -> Outcome {
    let handle = match open(pid, PROCESS_TERMINATE | PROCESS_SYNCHRONIZE) {
        Ok(handle) => handle,
        Err(outcome) => return outcome,
    };
    // SAFETY: the handle has PROCESS_TERMINATE access and is valid.
    match unsafe { TerminateProcess(handle.0, 1) } {
        Ok(()) => {
            if wait_for_exit(&handle, FORCE_WAIT_MS) {
                Outcome::Terminated
            } else {
                Outcome::Failed("the process did not exit after being terminated".to_string())
            }
        }
        Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => Outcome::AccessDenied,
        Err(e) => Outcome::Failed(e.message()),
    }
}

struct CloseRequest {
    pid: u32,
    sent: bool,
}

unsafe extern "system" fn close_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the address of the CloseRequest owned by `request_close`, alive during
    // the whole EnumWindows call.
    let request = unsafe { &mut *(lparam.0 as *mut CloseRequest) };
    let mut owner = 0u32;
    // SAFETY: `hwnd` is a window handle handed to us by EnumWindows.
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut owner));
        if owner == request.pid
            && IsWindowVisible(hwnd).as_bool()
            && PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok()
        {
            request.sent = true;
        }
    }
    true.into()
}

/// Asks every visible top-level window of the process to close. Returns true if at least one
/// request was sent (console and background processes have no window, so nothing is sent).
fn request_close(pid: u32) -> bool {
    let mut request = CloseRequest { pid, sent: false };
    // SAFETY: the callback only dereferences `lparam` as the CloseRequest passed here.
    unsafe {
        let _ = EnumWindows(
            Some(close_window),
            LPARAM(&mut request as *mut CloseRequest as isize),
        );
    }
    request.sent
}

/// Descendants of `root` (children, grandchildren...), ignoring "children" that started before
/// their parent, which would mean the parent PID was recycled.
fn descendants(system: &System, root: u32) -> Vec<u32> {
    let mut by_parent: HashMap<u32, Vec<(u32, u64)>> = HashMap::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            by_parent
                .entry(parent.as_u32())
                .or_default()
                .push((pid.as_u32(), process.start_time()));
        }
    }
    let mut found = Vec::new();
    let mut seen = HashSet::from([root]);
    let mut queue = vec![(
        root,
        system
            .process(Pid::from_u32(root))
            .map_or(0, |p| p.start_time()),
    )];
    while let Some((parent, parent_start)) = queue.pop() {
        for (child, child_start) in by_parent.get(&parent).into_iter().flatten() {
            if *child_start >= parent_start && seen.insert(*child) {
                found.push(*child);
                queue.push((*child, *child_start));
            }
        }
    }
    found
}

impl ProcessControl for WindowsControl {
    fn stop(&self, target: &PlannedAction, grace_ms: u64) -> Outcome {
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        match system.process(Pid::from_u32(target.pid)) {
            None => return Outcome::AlreadyGone,
            Some(process) if process.start_time() != target.start_time => {
                return Outcome::IdentityMismatch;
            }
            Some(_) => {}
        }

        if target.tree {
            for child in descendants(&system, target.pid) {
                // Best effort: a descendant that cannot be stopped does not block the main target.
                let _ = force_terminate(child);
            }
        }

        if target.action == ProcessAction::Terminate && request_close(target.pid) {
            let timeout = u32::try_from(grace_ms).unwrap_or(u32::MAX);
            if let Ok(handle) = open(target.pid, PROCESS_SYNCHRONIZE)
                && wait_for_exit(&handle, timeout)
            {
                return Outcome::Terminated;
            }
        }
        force_terminate(target.pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procs::read_process_meta;
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    /// A harmless long-running child created by the test itself. Killed on drop as a safety net.
    struct TestChild(Child);

    impl Drop for TestChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn spawn_sleeper() -> TestChild {
        let child = Command::new("ping")
            .args(["-n", "300", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .expect("failed to start ping");
        TestChild(child)
    }

    fn target_for(pid: u32, action: ProcessAction, tree: bool) -> PlannedAction {
        // The child may need a moment to show up in the process list.
        let mut start_time = None;
        for _ in 0..50 {
            if let Some(meta) = read_process_meta().get(&pid) {
                start_time = Some(meta.start_time);
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        PlannedAction {
            pid,
            name: "ping.exe".to_string(),
            start_time: start_time.expect("the child never appeared in the process list"),
            rule: "test".to_string(),
            action,
            tree,
            reclaimable_bytes: 0,
        }
    }

    #[test]
    fn terminates_a_process_without_windows_and_then_reports_it_gone() {
        let mut child = spawn_sleeper();
        let target = target_for(child.0.id(), ProcessAction::Terminate, false);
        assert_eq!(WindowsControl.stop(&target, 500), Outcome::Terminated);
        assert!(child.0.wait().is_ok());
        assert_eq!(WindowsControl.stop(&target, 500), Outcome::AlreadyGone);
    }

    #[test]
    fn kill_stops_the_process_immediately() {
        let mut child = spawn_sleeper();
        let target = target_for(child.0.id(), ProcessAction::Kill, false);
        assert_eq!(WindowsControl.stop(&target, 500), Outcome::Terminated);
        assert!(child.0.wait().is_ok());
    }

    #[test]
    fn a_wrong_start_time_means_the_pid_was_recycled_and_nothing_is_touched() {
        let mut child = spawn_sleeper();
        let mut target = target_for(child.0.id(), ProcessAction::Kill, false);
        target.start_time += 1;
        assert_eq!(WindowsControl.stop(&target, 500), Outcome::IdentityMismatch);
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "the process must still be running"
        );
    }

    #[test]
    fn tree_termination_also_stops_the_descendants() {
        // cmd starts ping as a child process.
        let mut parent = TestChild(
            Command::new("cmd")
                .args(["/c", "ping", "-n", "300", "127.0.0.1"])
                .stdout(Stdio::null())
                .spawn()
                .expect("failed to start cmd"),
        );
        let parent_pid = parent.0.id();
        let target = target_for(parent_pid, ProcessAction::Kill, true);

        let mut grandchild = None;
        for _ in 0..50 {
            grandchild = read_process_meta()
                .iter()
                .find(|(_, m)| m.parent_pid == Some(parent_pid))
                .map(|(pid, _)| *pid);
            if grandchild.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let grandchild = grandchild.expect("cmd never started its child");

        assert_eq!(WindowsControl.stop(&target, 500), Outcome::Terminated);
        assert!(parent.0.wait().is_ok());
        assert!(
            !read_process_meta().contains_key(&grandchild),
            "the descendant must be gone"
        );
    }
}
