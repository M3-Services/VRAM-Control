//! TUI state and key handling. Pure: no terminal and no operating system access, so that every
//! behavior can be unit-tested. Rendering is in `ui.rs` and the terminal loop in `tui.rs`.

use crate::config::Config;
use crate::executor::{ActionResult, render_report};
use crate::inventory::{AppGroup, Inventory, ProcessEntry, group_by_app};
use crate::plan::{Plan, PlannedAction, ProcessAction, ProtectedHit, render_plan};
use crate::protect::Protection;
use crate::rules::build_plan;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;

/// Rule name recorded in plans built from a manual selection.
pub const MANUAL_RULE: &str = "manual selection";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuTab {
    All,
    /// Position of the GPU in the inventory.
    Gpu(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Processes,
    /// Processes grouped by executable name (read-only view).
    Apps,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
    Confirm(Plan),
    Profiles { cursor: usize },
    Help,
    Results(String),
}

/// What the terminal loop must do after a key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    Quit,
    RefreshNow,
    Execute(Plan),
}

/// One displayed process line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub pid: u32,
    pub name: String,
    pub dedicated: u64,
    pub shared: u64,
    /// None when the start time could not be read (some system processes).
    pub age_secs: Option<u64>,
    /// GPU indexes the process uses, for example "0,1".
    pub gpus: String,
    pub protected: bool,
    pub suspect: bool,
    pub selected: bool,
}

pub struct App {
    pub inventory: Inventory,
    pub config: Option<Config>,
    /// Shown in the footer when there is no status message (for example "no configuration file").
    pub config_note: Option<String>,
    pub protection: Protection,
    pub now_unix: u64,
    pub tab: GpuTab,
    pub view: View,
    pub filter: String,
    pub mode: Mode,
    /// Selected processes: PID -> start time (so a recycled PID is never kept selected).
    pub selected: HashMap<u32, u64>,
    pub cursor: usize,
    pub status: Option<String>,
}

/// "45s", "12m", "3h 12m", "2d 4h".
pub fn format_age(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{secs}s")
    }
}

/// Builds a plan for processes picked by hand. Protected processes are reported, never planned.
pub fn plan_from_pids(inventory: &Inventory, pids: &[u32], protection: &Protection) -> Plan {
    let mut plan = Plan::default();
    for process in inventory.processes.iter().filter(|p| pids.contains(&p.pid)) {
        if protection.is_protected(process.pid, &process.name) {
            plan.protected.push(ProtectedHit {
                pid: process.pid,
                name: process.name.clone(),
                rule: MANUAL_RULE.to_string(),
            });
            continue;
        }
        plan.actions.push(PlannedAction {
            pid: process.pid,
            name: process.name.clone(),
            start_time: process.start_time,
            rule: MANUAL_RULE.to_string(),
            action: ProcessAction::Terminate,
            tree: false,
            reclaimable_bytes: process
                .usage
                .iter()
                .filter(|u| !u.suspect)
                .map(|u| u.dedicated_bytes)
                .sum(),
        });
    }
    plan
}

impl App {
    pub fn new(
        inventory: Inventory,
        config: Option<Config>,
        config_note: Option<String>,
        protection: Protection,
        now_unix: u64,
    ) -> Self {
        Self {
            inventory,
            config,
            config_note,
            protection,
            now_unix,
            tab: GpuTab::All,
            view: View::Processes,
            filter: String::new(),
            mode: Mode::Normal,
            selected: HashMap::new(),
            cursor: 0,
            status: None,
        }
    }

    pub fn is_multi_gpu(&self) -> bool {
        self.inventory.gpus.len() > 1
    }

    /// Replaces the inventory after a refresh, dropping selections that no longer apply.
    pub fn set_inventory(&mut self, inventory: Inventory, now_unix: u64) {
        self.inventory = inventory;
        self.now_unix = now_unix;
        let current: HashMap<u32, u64> = self
            .inventory
            .processes
            .iter()
            .map(|p| (p.pid, p.start_time))
            .collect();
        self.selected
            .retain(|pid, start| current.get(pid) == Some(start));
        if let GpuTab::Gpu(i) = self.tab
            && i >= self.inventory.gpus.len()
        {
            self.tab = GpuTab::All;
        }
        self.clamp_cursor();
    }

    pub fn profile_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .config
            .as_ref()
            .map(|c| c.profiles.keys().cloned().collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    fn matches_filter(&self, process: &ProcessEntry) -> bool {
        if self.filter.is_empty() {
            return true;
        }
        let needle = self.filter.to_lowercase();
        process.name.to_lowercase().contains(&needle)
            || process.cmdline.to_lowercase().contains(&needle)
            || process.pid.to_string().contains(&needle)
    }

    /// Processes visible under the current GPU tab and filter, as table rows.
    pub fn rows(&self) -> Vec<Row> {
        let tab_luid = match self.tab {
            GpuTab::All => None,
            GpuTab::Gpu(i) => self.inventory.gpus.get(i).map(|g| g.gpu.luid),
        };
        let mut rows: Vec<Row> = self
            .inventory
            .processes
            .iter()
            .filter(|p| self.matches_filter(p))
            .filter_map(|p| {
                let usages: Vec<_> = p
                    .usage
                    .iter()
                    .filter(|u| tab_luid.is_none_or(|l| u.luid == l))
                    .collect();
                if usages.is_empty() {
                    return None;
                }
                let mut gpu_ids: Vec<String> = p
                    .usage
                    .iter()
                    .filter_map(|u| {
                        self.inventory
                            .gpus
                            .iter()
                            .find(|g| g.gpu.luid == u.luid)
                            .map(|g| g.gpu.index.to_string())
                    })
                    .collect();
                gpu_ids.dedup();
                Some(Row {
                    pid: p.pid,
                    name: p.name.clone(),
                    dedicated: usages.iter().map(|u| u.dedicated_bytes).sum(),
                    shared: usages.iter().map(|u| u.shared_bytes).sum(),
                    age_secs: (p.start_time != 0)
                        .then(|| self.now_unix.saturating_sub(p.start_time)),
                    gpus: gpu_ids.join(","),
                    protected: self.protection.is_protected(p.pid, &p.name),
                    suspect: usages.iter().any(|u| u.suspect),
                    selected: self.selected.get(&p.pid) == Some(&p.start_time),
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            a.suspect
                .cmp(&b.suspect)
                .then(b.dedicated.cmp(&a.dedicated))
                .then(a.pid.cmp(&b.pid))
        });
        rows
    }

    /// Visible processes grouped by executable name.
    pub fn groups(&self) -> Vec<AppGroup> {
        let visible: Vec<ProcessEntry> = self
            .inventory
            .processes
            .iter()
            .filter(|p| self.matches_filter(p))
            .cloned()
            .collect();
        group_by_app(&visible)
    }

    fn visible_len(&self) -> usize {
        match self.view {
            View::Processes => self.rows().len(),
            View::Apps => self.groups().len(),
        }
    }

    fn clamp_cursor(&mut self) {
        let len = self.visible_len();
        self.cursor = if len == 0 {
            0
        } else {
            self.cursor.min(len - 1)
        };
    }

    pub fn handle_key(&mut self, event: KeyEvent) -> Effect {
        match self.mode.clone() {
            Mode::Normal => self.key_normal(event),
            Mode::Filter => self.key_filter(event),
            Mode::Confirm(plan) => self.key_confirm(event, plan),
            Mode::Profiles { cursor } => self.key_profiles(event, cursor),
            Mode::Help | Mode::Results(_) => {
                self.mode = Mode::Normal;
                Effect::None
            }
        }
    }

    fn key_normal(&mut self, event: KeyEvent) -> Effect {
        self.status = None;
        if event.modifiers.contains(KeyModifiers::CONTROL) && event.code == KeyCode::Char('c') {
            return Effect::Quit;
        }
        let page = 10;
        let last = self.visible_len().saturating_sub(1);
        match event.code {
            KeyCode::Char('q') => return Effect::Quit,
            KeyCode::Esc => {
                if self.filter.is_empty() {
                    return Effect::Quit;
                }
                self.filter.clear();
                self.clamp_cursor();
            }
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down => self.cursor = (self.cursor + 1).min(last),
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(page),
            KeyCode::PageDown => self.cursor = (self.cursor + page).min(last),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = last,
            KeyCode::Char(' ') => self.toggle_current(),
            KeyCode::Char('a') => self.toggle_all_visible(),
            KeyCode::Char('/') => self.mode = Mode::Filter,
            KeyCode::Char('g') => {
                self.view = match self.view {
                    View::Processes => View::Apps,
                    View::Apps => View::Processes,
                };
                self.cursor = 0;
            }
            KeyCode::Tab => self.next_tab(1),
            KeyCode::BackTab => self.next_tab(-1),
            KeyCode::Char('r') => return Effect::RefreshNow,
            KeyCode::Char('k') => self.start_kill(),
            KeyCode::Char('p') => self.start_profiles(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
        Effect::None
    }

    fn key_filter(&mut self, event: KeyEvent) -> Effect {
        match event.code {
            KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Esc => {
                self.filter.clear();
                self.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char(c) => self.filter.push(c),
            _ => {}
        }
        self.clamp_cursor();
        Effect::None
    }

    fn key_confirm(&mut self, event: KeyEvent, plan: Plan) -> Effect {
        self.mode = Mode::Normal;
        match event.code {
            KeyCode::Char('y') | KeyCode::Char('Y') if !plan.actions.is_empty() => {
                Effect::Execute(plan)
            }
            _ => {
                self.status = Some("Cancelled: nothing was changed.".to_string());
                Effect::None
            }
        }
    }

    fn key_profiles(&mut self, event: KeyEvent, cursor: usize) -> Effect {
        let names = self.profile_names();
        match event.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Up => {
                self.mode = Mode::Profiles {
                    cursor: cursor.saturating_sub(1),
                }
            }
            KeyCode::Down => {
                self.mode = Mode::Profiles {
                    cursor: (cursor + 1).min(names.len().saturating_sub(1)),
                }
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                let (Some(name), Some(config)) = (names.get(cursor), self.config.as_ref()) else {
                    return Effect::None;
                };
                match build_plan(&self.inventory, config, Some(name), &self.protection) {
                    Ok(plan) => self.open_confirm(plan, &format!("profile '{name}'")),
                    Err(e) => self.status = Some(format!("{e:#}")),
                }
            }
            _ => {}
        }
        Effect::None
    }

    fn open_confirm(&mut self, plan: Plan, what: &str) {
        if plan.actions.is_empty() && plan.protected.is_empty() {
            self.status = Some(format!("Nothing to do for {what}."));
        } else {
            self.mode = Mode::Confirm(plan);
        }
    }

    fn toggle_current(&mut self) {
        if self.view == View::Apps {
            self.status = Some("Switch back to the process view (g) to select processes.".into());
            return;
        }
        let rows = self.rows();
        let Some(row) = rows.get(self.cursor) else {
            return;
        };
        let Some(process) = self.inventory.processes.iter().find(|p| p.pid == row.pid) else {
            return;
        };
        if self.selected.remove(&row.pid).is_none() {
            self.selected.insert(row.pid, process.start_time);
        }
    }

    fn toggle_all_visible(&mut self) {
        if self.view == View::Apps {
            return;
        }
        let rows = self.rows();
        let all_selected = !rows.is_empty() && rows.iter().all(|r| r.selected);
        for row in &rows {
            if all_selected {
                self.selected.remove(&row.pid);
            } else if let Some(p) = self.inventory.processes.iter().find(|p| p.pid == row.pid) {
                self.selected.insert(row.pid, p.start_time);
            }
        }
    }

    fn next_tab(&mut self, step: isize) {
        let gpus = self.inventory.gpus.len();
        if gpus < 2 {
            return;
        }
        // Positions: 0 = All, 1..=gpus = each GPU.
        let current = match self.tab {
            GpuTab::All => 0,
            GpuTab::Gpu(i) => i + 1,
        } as isize;
        let next = (current + step).rem_euclid(gpus as isize + 1) as usize;
        self.tab = if next == 0 {
            GpuTab::All
        } else {
            GpuTab::Gpu(next - 1)
        };
        self.cursor = 0;
    }

    fn start_kill(&mut self) {
        if self.view == View::Apps {
            self.status =
                Some("Switch back to the process view (g) to terminate processes.".into());
            return;
        }
        let mut pids: Vec<u32> = self.selected.keys().copied().collect();
        if pids.is_empty() {
            match self.rows().get(self.cursor) {
                Some(row) => pids.push(row.pid),
                None => return,
            }
        }
        let plan = plan_from_pids(&self.inventory, &pids, &self.protection);
        self.open_confirm(plan, "the selection");
    }

    fn start_profiles(&mut self) {
        if self.config.is_none() {
            self.status = Some("No configuration loaded: profiles are unavailable.".into());
        } else if self.profile_names().is_empty() {
            self.status = Some("The configuration defines no profile.".into());
        } else {
            self.mode = Mode::Profiles { cursor: 0 };
        }
    }

    /// Shows the outcome of an executed plan and clears the selection.
    pub fn apply_results(&mut self, results: &[ActionResult], expected: u64, freed: Option<u64>) {
        self.selected.clear();
        self.mode = Mode::Results(render_report(results, expected, freed));
    }

    /// The confirmation popup text for a pending plan.
    pub fn confirm_text(plan: &Plan) -> String {
        let mut text = render_plan(plan);
        if !plan.actions.is_empty() {
            text.push_str(
                "\nTerminate these processes? Press y to confirm, any other key cancels.",
            );
        } else {
            text.push_str("\nNothing can be terminated. Press any key.");
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::build_inventory;
    use crate::model::{AdapterSample, GpuInfo, Luid, ProcessMeta, ProcessSample};

    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn gpu(index: usize, low: u32) -> GpuInfo {
        GpuInfo {
            index,
            name: format!("Test GPU {index}"),
            luid: Luid::new(0, low),
            total_bytes: 24 * GIB,
        }
    }

    /// (pid, name, cmdline, gpu luid low part, dedicated bytes)
    type Spec<'a> = (u32, &'a str, &'a str, u32, u64);

    fn inventory(gpus: &[GpuInfo], processes: &[Spec]) -> Inventory {
        let adapters: Vec<AdapterSample> = gpus
            .iter()
            .map(|g| AdapterSample {
                luid: g.luid,
                dedicated_used_bytes: 10 * GIB,
            })
            .collect();
        let samples: Vec<ProcessSample> = processes
            .iter()
            .map(|(pid, _, _, low, bytes)| ProcessSample {
                pid: *pid,
                luid: Luid::new(0, *low),
                dedicated_bytes: *bytes,
                shared_bytes: 0,
            })
            .collect();
        let metas: HashMap<u32, ProcessMeta> = processes
            .iter()
            .map(|(pid, name, cmd, _, _)| {
                (
                    *pid,
                    ProcessMeta {
                        name: name.to_string(),
                        cmdline: cmd.to_string(),
                        parent_pid: None,
                        start_time: 1000 + u64::from(*pid),
                    },
                )
            })
            .collect();
        build_inventory(gpus, &adapters, &samples, &metas)
    }

    fn app_with(gpus: &[GpuInfo], processes: &[Spec], config: Option<&str>) -> App {
        App::new(
            inventory(gpus, processes),
            config.map(|c| Config::parse(c).unwrap()),
            None,
            Protection::new(&[], 99_999),
            2000,
        )
    }

    fn one_gpu_app() -> App {
        app_with(
            &[gpu(0, 1)],
            &[
                (
                    10,
                    "llama-server.exe",
                    "llama-server --model m.gguf",
                    1,
                    6 * GIB,
                ),
                (11, "python.exe", "python train.py", 1, 2 * GIB),
                (12, "tiny.exe", "", 1, 5 * MIB),
                (500, "dwm.exe", "", 1, GIB),
            ],
            Some(PROFILE_CONFIG),
        )
    }

    const PROFILE_CONFIG: &str = r#"
[[rule]]
name = "llama"
platform = "windows"
match = { name = "llama-server.exe" }
action = "terminate"

[profiles]
alpha = ["llama"]
beta = ["llama"]
"#;

    fn press(app: &mut App, code: KeyCode) -> Effect {
        app.handle_key(key(code))
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn formats_ages() {
        assert_eq!(format_age(5), "5s");
        assert_eq!(format_age(125), "2m");
        assert_eq!(format_age(3 * 3600 + 12 * 60), "3h 12m");
        assert_eq!(format_age(2 * 86_400 + 4 * 3600), "2d 4h");
    }

    #[test]
    fn rows_are_sorted_by_vram_and_carry_age_and_protection() {
        let app = one_gpu_app();
        let rows = app.rows();
        let pids: Vec<u32> = rows.iter().map(|r| r.pid).collect();
        assert_eq!(pids, vec![10, 11, 500, 12]);
        assert_eq!(rows[0].age_secs, Some(2000 - 1010));
        assert!(rows.iter().find(|r| r.pid == 500).unwrap().protected);
        assert!(!rows[0].protected);
    }

    #[test]
    fn an_unreadable_start_time_means_an_unknown_age() {
        let mut app = one_gpu_app();
        app.inventory
            .processes
            .iter_mut()
            .find(|p| p.pid == 10)
            .unwrap()
            .start_time = 0;
        let rows = app.rows();
        assert_eq!(rows.iter().find(|r| r.pid == 10).unwrap().age_secs, None);
        assert!(
            rows.iter()
                .find(|r| r.pid == 11)
                .unwrap()
                .age_secs
                .is_some()
        );
    }

    #[test]
    fn the_filter_matches_name_command_line_and_pid() {
        let mut app = one_gpu_app();
        press(&mut app, KeyCode::Char('/'));
        assert_eq!(app.mode, Mode::Filter);
        type_text(&mut app, "TRAIN");
        assert_eq!(
            app.rows().iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![11]
        );
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        type_text(&mut app, "10");
        assert_eq!(
            app.rows().iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![10]
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.filter, "10");
        press(&mut app, KeyCode::Esc);
        assert!(app.filter.is_empty());
        assert_eq!(app.rows().len(), 4);
    }

    #[test]
    fn the_cursor_moves_within_bounds() {
        let mut app = one_gpu_app();
        press(&mut app, KeyCode::Up);
        assert_eq!(app.cursor, 0);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.cursor, 2);
        press(&mut app, KeyCode::End);
        assert_eq!(app.cursor, 3);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.cursor, 3);
        press(&mut app, KeyCode::Home);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn space_toggles_the_selection_and_a_selects_everything_visible() {
        let mut app = one_gpu_app();
        press(&mut app, KeyCode::Char(' '));
        assert!(app.rows()[0].selected);
        press(&mut app, KeyCode::Char(' '));
        assert!(!app.rows()[0].selected);
        press(&mut app, KeyCode::Char('a'));
        assert!(app.rows().iter().all(|r| r.selected));
        press(&mut app, KeyCode::Char('a'));
        assert!(app.rows().iter().all(|r| !r.selected));
    }

    #[test]
    fn a_refresh_drops_selections_of_vanished_or_recycled_processes() {
        let mut app = one_gpu_app();
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.selected.len(), 4);
        // PID 11 is gone; PID 12 now has a different start time (recycled).
        let mut fresh = inventory(
            &[gpu(0, 1)],
            &[
                (10, "llama-server.exe", "", 1, GIB),
                (12, "other.exe", "", 1, GIB),
            ],
        );
        fresh
            .processes
            .iter_mut()
            .find(|p| p.pid == 12)
            .unwrap()
            .start_time = 7;
        app.set_inventory(fresh, 3000);
        let mut kept: Vec<u32> = app.selected.keys().copied().collect();
        kept.sort();
        assert_eq!(kept, vec![10]);
    }

    #[test]
    fn k_without_a_selection_targets_the_row_under_the_cursor() {
        let mut app = one_gpu_app();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('k'));
        let Mode::Confirm(plan) = &app.mode else {
            panic!("expected a confirmation, got {:?}", app.mode);
        };
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].pid, 11);
        assert_eq!(plan.actions[0].rule, MANUAL_RULE);
        assert_eq!(plan.actions[0].action, ProcessAction::Terminate);
    }

    #[test]
    fn protected_processes_are_refused_and_never_executed() {
        let mut app = one_gpu_app();
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('k'));
        let Mode::Confirm(plan) = app.mode.clone() else {
            panic!("expected a confirmation");
        };
        assert_eq!(plan.protected.len(), 1);
        assert_eq!(plan.protected[0].pid, 500);
        assert!(plan.actions.iter().all(|a| a.pid != 500));
        assert_eq!(press(&mut app, KeyCode::Char('y')), Effect::Execute(plan));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn only_y_confirms_and_anything_else_cancels() {
        for code in [
            KeyCode::Char('n'),
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Char(' '),
        ] {
            let mut app = one_gpu_app();
            press(&mut app, KeyCode::Char('k'));
            assert!(matches!(app.mode, Mode::Confirm(_)));
            assert_eq!(press(&mut app, code), Effect::None);
            assert_eq!(app.mode, Mode::Normal);
            assert!(app.status.as_deref().unwrap().contains("Cancelled"));
        }
    }

    #[test]
    fn a_plan_made_only_of_protected_processes_cannot_be_executed() {
        let mut app = one_gpu_app();
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "dwm");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('k'));
        assert!(matches!(app.mode, Mode::Confirm(_)));
        assert_eq!(press(&mut app, KeyCode::Char('y')), Effect::None);
    }

    #[test]
    fn the_app_view_is_read_only_for_selection_and_termination() {
        let mut app = one_gpu_app();
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.view, View::Apps);
        assert!(!app.groups().is_empty());
        press(&mut app, KeyCode::Char(' '));
        assert!(app.selected.is_empty());
        assert!(app.status.as_deref().unwrap().contains("process view"));
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.mode, Mode::Normal);
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.view, View::Processes);
    }

    #[test]
    fn tabs_only_exist_with_several_gpus_and_filter_the_rows() {
        let mut single = one_gpu_app();
        press(&mut single, KeyCode::Tab);
        assert_eq!(single.tab, GpuTab::All);

        let mut app = app_with(
            &[gpu(0, 1), gpu(1, 2)],
            &[(10, "a.exe", "", 1, GIB), (11, "b.exe", "", 2, 2 * GIB)],
            None,
        );
        assert!(app.is_multi_gpu());
        assert_eq!(app.rows().len(), 2);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.tab, GpuTab::Gpu(0));
        assert_eq!(
            app.rows().iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![10]
        );
        press(&mut app, KeyCode::Tab);
        assert_eq!(
            app.rows().iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![11]
        );
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.tab, GpuTab::All);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.tab, GpuTab::Gpu(1));
    }

    #[test]
    fn p_opens_the_profile_picker_and_enter_builds_a_plan() {
        let mut app = one_gpu_app();
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.mode, Mode::Profiles { cursor: 0 });
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.mode, Mode::Profiles { cursor: 1 });
        press(&mut app, KeyCode::Enter);
        let Mode::Confirm(plan) = &app.mode else {
            panic!("expected a confirmation");
        };
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].pid, 10);
        assert_eq!(plan.actions[0].rule, "llama");
    }

    #[test]
    fn p_explains_when_there_is_no_config_or_no_profile() {
        let mut none = app_with(&[gpu(0, 1)], &[(10, "a.exe", "", 1, GIB)], None);
        press(&mut none, KeyCode::Char('p'));
        assert!(none.status.as_deref().unwrap().contains("No configuration"));
        let mut empty = app_with(&[gpu(0, 1)], &[(10, "a.exe", "", 1, GIB)], Some(""));
        press(&mut empty, KeyCode::Char('p'));
        assert!(empty.status.as_deref().unwrap().contains("no profile"));
    }

    #[test]
    fn a_profile_that_matches_nothing_says_so() {
        let mut app = app_with(
            &[gpu(0, 1)],
            &[(11, "python.exe", "", 1, GIB)],
            Some(PROFILE_CONFIG),
        );
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.status.as_deref().unwrap().contains("Nothing to do"));
    }

    #[test]
    fn q_and_ctrl_c_quit_and_r_asks_for_a_refresh() {
        let mut app = one_gpu_app();
        assert_eq!(press(&mut app, KeyCode::Char('r')), Effect::RefreshNow);
        assert_eq!(press(&mut app, KeyCode::Char('q')), Effect::Quit);
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Effect::Quit
        );
        assert_eq!(press(&mut app, KeyCode::Esc), Effect::Quit);
    }

    #[test]
    fn help_and_results_close_on_any_key() {
        let mut app = one_gpu_app();
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.mode, Mode::Help);
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.mode, Mode::Normal);
        app.apply_results(&[], 0, None);
        assert!(matches!(app.mode, Mode::Results(_)));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn the_confirmation_text_distinguishes_executable_and_refused_plans() {
        let app = one_gpu_app();
        let ok = plan_from_pids(&app.inventory, &[10], &app.protection);
        assert!(App::confirm_text(&ok).contains("Press y to confirm"));
        let refused = plan_from_pids(&app.inventory, &[500], &app.protection);
        assert!(App::confirm_text(&refused).contains("Nothing can be terminated"));
    }
}
