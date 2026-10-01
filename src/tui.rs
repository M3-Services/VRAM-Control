//! The terminal loop: owns the terminal, refreshes the inventory on a background thread and runs
//! confirmed plans. All decisions live in `app.rs`; all drawing lives in `ui.rs`.

use crate::app::{App, Effect};
use crate::collect::collect_inventory;
use crate::config::Config;
use crate::executor::execute_plan;
use crate::inventory::Inventory;
use crate::plan::Plan;
use crate::protect::Protection;
use crate::ui::draw;
use crate::winproc::WindowsControl;
use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How often the loop wakes up to redraw and look for new inventory data.
const POLL: Duration = Duration::from_millis(100);

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn used_bytes(inventory: &Inventory) -> u64 {
    inventory.gpus.iter().map(|g| g.used_bytes).sum()
}

/// Collects the inventory periodically on its own thread so that a slow read never freezes the UI.
struct Collector {
    results: Receiver<Result<Inventory, String>>,
    /// Asks for an immediate refresh.
    nudge: Sender<()>,
}

fn spawn_collector(refresh: Duration) -> Collector {
    let (result_tx, results) = mpsc::channel();
    let (nudge, nudge_rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        // A timeout means "refresh on schedule"; a nudge means "refresh now"; a disconnection
        // means the UI is gone.
        while !matches!(
            nudge_rx.recv_timeout(refresh),
            Err(RecvTimeoutError::Disconnected)
        ) {
            let message = collect_inventory().map_err(|e| format!("{e:#}"));
            if result_tx.send(message).is_err() {
                break;
            }
        }
    });
    Collector { results, nudge }
}

/// The TUI must work without a configuration: this never fails, it returns a note instead.
fn load_config(path: &Path) -> (Option<Config>, Option<String>) {
    if !path.exists() {
        let note = format!(
            "No configuration file at {}: rules and profiles are unavailable.",
            path.display()
        );
        return (None, Some(note));
    }
    match Config::load(path) {
        Ok(config) => (Some(config), None),
        Err(e) => {
            let message = format!("{e:#}").replace('\n', " ");
            (None, Some(format!("Configuration error: {message}")))
        }
    }
}

pub fn run(config_path: &Path) -> Result<()> {
    let (config, note) = load_config(config_path);
    let extra = config
        .as_ref()
        .map(|c| c.protect.windows.clone())
        .unwrap_or_default();
    let settings = config
        .as_ref()
        .map(|c| c.settings.clone())
        .unwrap_or_default();
    let protection = Protection::new(&extra, std::process::id());

    // Read once before touching the terminal, so a missing GPU shows a normal error message.
    let initial = collect_inventory()?;
    let mut app = App::new(initial, config, note, protection, now_unix());
    let collector = spawn_collector(Duration::from_millis(settings.refresh_ms));

    let mut terminal = ratatui::try_init()?;
    let result = event_loop(&mut terminal, &mut app, &collector, settings.grace_ms);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    collector: &Collector,
    grace_ms: u64,
) -> Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, app))?;

        if event::poll(POLL)?
            && let Event::Key(key) = event::read()?
            // Windows reports both key presses and releases.
            && key.kind == KeyEventKind::Press
        {
            match app.handle_key(key) {
                Effect::Quit => return Ok(()),
                Effect::RefreshNow => {
                    let _ = collector.nudge.send(());
                }
                Effect::Execute(plan) => {
                    app.status = Some("Terminating...".to_string());
                    terminal.draw(|frame| draw(frame, app))?;
                    execute(app, &plan, grace_ms);
                }
                Effect::None => {}
            }
        }

        while let Ok(message) = collector.results.try_recv() {
            match message {
                Ok(inventory) => app.set_inventory(inventory, now_unix()),
                Err(e) => app.status = Some(format!("Refresh failed: {e}")),
            }
        }
    }
}

/// Runs a confirmed plan, then measures what was actually freed and shows the report.
fn execute(app: &mut App, plan: &Plan, grace_ms: u64) {
    let before = used_bytes(&app.inventory);
    let results = execute_plan(plan, &WindowsControl, grace_ms);
    // Give the driver a moment to release the memory.
    std::thread::sleep(Duration::from_millis(500));
    let freed = match collect_inventory() {
        Ok(inventory) => {
            let freed = before.saturating_sub(used_bytes(&inventory));
            app.set_inventory(inventory, now_unix());
            Some(freed)
        }
        Err(_) => None,
    };
    app.status = None;
    app.apply_results(&results, plan.total_reclaimable(), freed);
}
