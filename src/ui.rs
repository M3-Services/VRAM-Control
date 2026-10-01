//! Rendering of the TUI with ratatui. Reads the [`App`] state and draws; never changes it.

use crate::app::{App, GpuTab, Mode, View, format_age};
use crate::units::format_bytes;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Row, Table, TableState,
    Tabs, Wrap,
};

const HELP: &str = "\
Up/Down, PageUp/PageDown, Home/End   move
Enter                                details of the process (full path, command line, parent, user...)
Space                                select / unselect the process
a                                    select / unselect everything visible
k                                    terminate the selection (or the process under the cursor)
p                                    apply a profile from the configuration
/                                    filter (name, command line or PID); Enter keeps it, Esc clears it
g                                    switch between processes and applications
Tab / Shift+Tab                      next / previous GPU (several GPUs only)
r                                    refresh now
?                                    this help
q or Esc                             quit

Nothing is terminated without a confirmation. Protected processes are always refused.";

fn usage_color(ratio: f64) -> Color {
    if ratio >= 0.9 {
        Color::Red
    } else if ratio >= 0.7 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn centered(area: Rect, percent_x: u16, height: u16) -> Rect {
    let height = height.min(area.height);
    let width = area.width * percent_x / 100;
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

/// Number of screen lines `text` needs once wrapped to `width` columns.
fn wrapped_height(text: &str, width: u16) -> u16 {
    let width = usize::from(width.max(1));
    let lines: usize = text
        .lines()
        .map(|line| line.chars().count().max(1).div_ceil(width))
        .sum();
    u16::try_from(lines).unwrap_or(u16::MAX)
}

fn popup(frame: &mut Frame, title: &str, text: &str) {
    // 80% of the width, minus the two border columns, is what the text can use.
    let inner_width = (frame.area().width * 80 / 100).saturating_sub(2);
    let lines = wrapped_height(text, inner_width).saturating_add(2);
    let area = centered(frame.area(), 80, lines);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "));
    frame.render_widget(
        Paragraph::new(text.to_string())
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

pub fn draw(frame: &mut Frame, app: &App) {
    let gpu_lines = app.inventory.gpus.len() as u16;
    let tab_line = u16::from(app.is_multi_gpu());
    let [gpus_area, tabs_area, table_area, footer_area] = Layout::vertical([
        Constraint::Length(gpu_lines),
        Constraint::Length(tab_line),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_gpus(frame, app, gpus_area);
    if app.is_multi_gpu() {
        draw_tabs(frame, app, tabs_area);
    }
    match app.view {
        View::Processes => draw_processes(frame, app, table_area),
        View::Apps => draw_apps(frame, app, table_area),
    }
    draw_footer(frame, app, footer_area);

    match &app.mode {
        Mode::Confirm(plan) => popup(frame, "Confirm", &App::confirm_text(plan)),
        Mode::Help => popup(frame, "Help", HELP),
        Mode::Results(report) => popup(frame, "Result (press any key)", report),
        Mode::Details(text) => popup(frame, "Process details (press any key)", text),
        Mode::Profiles { cursor } => draw_profiles(frame, app, *cursor),
        Mode::Normal | Mode::Filter => {}
    }
}

fn draw_gpus(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical(vec![Constraint::Length(1); app.inventory.gpus.len()]).split(area);
    for (summary, row) in app.inventory.gpus.iter().zip(rows.iter()) {
        let total = summary.gpu.total_bytes.max(1);
        let ratio = (summary.used_bytes as f64 / total as f64).clamp(0.0, 1.0);
        let label = format!(
            "GPU {} {}  {} / {}  (unattributed {})",
            summary.gpu.index,
            summary.gpu.name,
            format_bytes(summary.used_bytes),
            format_bytes(summary.gpu.total_bytes),
            format_bytes(summary.unattributed_bytes),
        );
        frame.render_widget(
            Gauge::default()
                .ratio(ratio)
                .label(label)
                .gauge_style(Style::default().fg(usage_color(ratio)).bg(Color::DarkGray)),
            *row,
        );
    }
}

fn draw_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let mut titles = vec!["All GPUs".to_string()];
    titles.extend(
        app.inventory
            .gpus
            .iter()
            .map(|g| format!("GPU {}", g.gpu.index)),
    );
    let selected = match app.tab {
        GpuTab::All => 0,
        GpuTab::Gpu(i) => i + 1,
    };
    frame.render_widget(
        Tabs::new(titles)
            .select(selected)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
    );
}

fn table_title(app: &App, what: &str, count: usize) -> String {
    let mut title = format!(" {what} ({count})");
    if !app.filter.is_empty() {
        title.push_str(&format!("  filter: {}", app.filter));
    }
    if !app.selected.is_empty() {
        title.push_str(&format!("  {} selected", app.selected.len()));
    }
    title.push(' ');
    title
}

fn draw_processes(frame: &mut Frame, app: &App, area: Rect) {
    let rows = app.rows();
    let show_gpu = app.is_multi_gpu() && app.tab == GpuTab::All;
    let mut header = vec!["", "PID", "NAME", "DEDICATED", "SHARED", "AGE", "FLAGS"];
    let mut widths = vec![
        Constraint::Length(3),
        Constraint::Length(7),
        Constraint::Min(16),
        Constraint::Length(11),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(10),
    ];
    if show_gpu {
        header.push("GPU");
        widths.push(Constraint::Length(5));
    }
    let body: Vec<Row> = rows
        .iter()
        .map(|r| {
            let mut flags = Vec::new();
            if r.protected {
                flags.push("protected");
            }
            if r.suspect {
                flags.push("?");
            }
            let mut cells = vec![
                if r.selected {
                    "[x]".to_string()
                } else {
                    "[ ]".to_string()
                },
                r.pid.to_string(),
                r.name.clone(),
                format_bytes(r.dedicated),
                format_bytes(r.shared),
                r.age_secs.map_or_else(|| "-".to_string(), format_age),
                flags.join(" "),
            ];
            if show_gpu {
                cells.push(r.gpus.clone());
            }
            let style = if r.protected {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            Row::new(cells).style(style)
        })
        .collect();
    let table = Table::new(body, widths)
        .header(Row::new(header).style(Style::default().add_modifier(Modifier::BOLD)))
        .block(Block::default().borders(Borders::TOP).title(table_title(
            app,
            "Processes",
            rows.len(),
        )))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default();
    if !rows.is_empty() {
        state.select(Some(app.cursor));
    }
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_apps(frame: &mut Frame, app: &App, area: Rect) {
    let groups = app.groups();
    let body: Vec<Row> = groups
        .iter()
        .map(|g| {
            Row::new(vec![
                g.name.clone(),
                g.process_count.to_string(),
                format_bytes(g.dedicated_bytes),
                format_bytes(g.shared_bytes),
            ])
        })
        .collect();
    let table = Table::new(
        body,
        [
            Constraint::Min(20),
            Constraint::Length(7),
            Constraint::Length(11),
            Constraint::Length(10),
        ],
    )
    .header(
        Row::new(vec!["APPLICATION", "PROCS", "DEDICATED", "SHARED"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().borders(Borders::TOP).title(table_title(
        app,
        "Applications (read-only)",
        groups.len(),
    )))
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default();
    if !groups.is_empty() {
        state.select(Some(app.cursor));
    }
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_profiles(frame: &mut Frame, app: &App, cursor: usize) {
    let names = app.profile_names();
    let area = centered(frame.area(), 50, names.len() as u16 + 2);
    frame.render_widget(Clear, area);
    let items: Vec<ListItem> = names.iter().map(|n| ListItem::new(n.clone())).collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Profile (Enter to apply, Esc to cancel) "),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    state.select(Some(cursor));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let line = if app.mode == Mode::Filter {
        Line::from(vec![
            Span::styled("/", Style::default().fg(Color::Yellow)),
            Span::raw(app.filter.clone()),
            Span::raw("_"),
        ])
    } else if let Some(status) = &app.status {
        Line::from(Span::styled(
            status.clone(),
            Style::default().fg(Color::Yellow),
        ))
    } else if let Some(note) = &app.config_note {
        Line::from(Span::styled(
            note.clone(),
            Style::default().fg(Color::Yellow),
        ))
    } else {
        Line::from(
            "Space select  k terminate  p profile  / filter  g apps  Tab GPU  r refresh  ? help  q quit",
        )
    };
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Left), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::inventory::{Inventory, build_inventory};
    use crate::model::{AdapterSample, GpuInfo, Luid, ProcessMeta, ProcessSample};
    use crate::protect::Protection;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::collections::HashMap;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn inventory(gpu_count: usize) -> Inventory {
        let gpus: Vec<GpuInfo> = (0..gpu_count)
            .map(|i| GpuInfo {
                index: i,
                name: format!("Test GPU {i}"),
                luid: Luid::new(0, i as u32 + 1),
                total_bytes: 24 * GIB,
            })
            .collect();
        let adapters: Vec<AdapterSample> = gpus
            .iter()
            .map(|g| AdapterSample {
                luid: g.luid,
                dedicated_used_bytes: 10 * GIB,
            })
            .collect();
        let sample = |pid, low, bytes| ProcessSample {
            pid,
            luid: Luid::new(0, low),
            dedicated_bytes: bytes,
            shared_bytes: 0,
        };
        let samples = vec![
            sample(10, 1, 6 * GIB),
            sample(11, 1, 2 * GIB),
            sample(500, 1, GIB),
            sample(12, if gpu_count > 1 { 2 } else { 1 }, GIB / 2),
        ];
        let meta = |name: &str| ProcessMeta {
            name: name.to_string(),
            cmdline: String::new(),
            parent_pid: None,
            start_time: 1000,
        };
        let metas: HashMap<u32, ProcessMeta> = HashMap::from([
            (10, meta("llama-server.exe")),
            (11, meta("python.exe")),
            (500, meta("dwm.exe")),
            (12, meta("other.exe")),
        ]);
        build_inventory(&gpus, &adapters, &samples, &metas)
    }

    fn app(gpu_count: usize) -> App {
        let config = Config::parse(
            "[[rule]]\nname = \"r\"\nplatform = \"windows\"\nmatch = { name = \"a.exe\" }\naction = \"kill\"\n[profiles]\nfree-max = [\"r\"]\n",
        )
        .unwrap();
        App::new(
            inventory(gpu_count),
            Some(config),
            None,
            Protection::new(&[], 99_999),
            4600,
        )
    }

    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn draws_the_gpu_summary_and_the_process_table() {
        let text = render(&app(1), 110, 20);
        assert!(text.contains("GPU 0 Test GPU 0"));
        assert!(text.contains("10.0 GiB / 24.0 GiB"));
        assert!(text.contains("llama-server.exe"));
        assert!(text.contains("6.0 GiB"));
        assert!(text.contains("1h 0m"), "age column: {text}");
        assert!(text.contains("protected"));
        assert!(text.contains("Processes (4)"));
        assert!(!text.contains("All GPUs"));
        assert!(text.contains("Space select"));
    }

    #[test]
    fn selection_filter_and_status_are_visible() {
        let mut app = app(1);
        press(&mut app, KeyCode::Char(' '));
        assert!(render(&app, 110, 20).contains("[x]"));
        assert!(render(&app, 110, 20).contains("1 selected"));
        press(&mut app, KeyCode::Char('/'));
        for c in "py".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let text = render(&app, 110, 20);
        assert!(text.contains("/py_"));
        assert!(text.contains("filter: py"));
        assert!(text.contains("python.exe"));
        assert!(!text.contains("llama-server.exe"));
    }

    #[test]
    fn several_gpus_show_tabs_and_a_gpu_column() {
        let mut app = app(2);
        let text = render(&app, 110, 20);
        assert!(text.contains("All GPUs"));
        assert!(text.contains("GPU 1 Test GPU 1"));
        assert!(
            text.lines()
                .any(|l| l.contains("other.exe") && l.contains('1'))
        );
        press(&mut app, KeyCode::Tab);
        let text = render(&app, 110, 20);
        assert!(text.contains("llama-server.exe"));
        assert!(!text.contains("other.exe"));
    }

    #[test]
    fn the_apps_view_lists_groups() {
        let mut app = app(1);
        press(&mut app, KeyCode::Char('g'));
        let text = render(&app, 110, 20);
        assert!(text.contains("APPLICATION"));
        assert!(text.contains("Applications (read-only) (4)"));
    }

    #[test]
    fn popups_show_the_plan_the_help_the_profiles_and_the_results() {
        let mut app = app(1);
        press(&mut app, KeyCode::Char('k'));
        let text = render(&app, 110, 30);
        assert!(text.contains("Confirm"));
        assert!(text.contains("llama-server.exe"));
        assert!(text.contains("Press y to confirm"));

        let mut app = app_fresh();
        press(&mut app, KeyCode::Char('?'));
        assert!(render(&app, 110, 30).contains("terminate the selection"));

        let mut app = app_fresh();
        press(&mut app, KeyCode::Char('p'));
        let text = render(&app, 110, 30);
        assert!(text.contains("free-max"));
        assert!(text.contains("Profile"));

        let mut app = app_fresh();
        app.apply_results(&[], 0, Some(GIB));
        let text = render(&app, 110, 30);
        assert!(text.contains("Stopped 0 of 0"));
        assert!(text.contains("VRAM freed: 1.0 GiB"));
    }

    fn app_fresh() -> App {
        app(1)
    }

    #[test]
    fn the_details_popup_is_drawn_and_wraps_long_lines() {
        let mut app = app(1);
        let long_path = format!(r"C:\{}\tool.exe", "very-long-folder-name\\".repeat(6));
        let details = crate::model::ProcessDetails {
            exe: Some(long_path),
            cmdline: "llama-server --model m.gguf".to_string(),
            cwd: None,
            parent: Some((1, "parent.exe".to_string())),
            user: Some("someone".to_string()),
            ram_bytes: GIB,
        };
        app.show_details(10, Some(details));
        let text = render(&app, 100, 30);
        assert!(text.contains("Process details"));
        assert!(text.contains("llama-server.exe"));
        assert!(
            text.contains("tool.exe"),
            "the end of the long path must be visible: {text}"
        );
        assert!(text.contains("parent.exe (PID 1)"));
        assert!(text.contains("VRAM on GPU 0 Test GPU 0"));
    }

    #[test]
    fn wrapped_height_counts_wrapped_lines() {
        assert_eq!(wrapped_height("", 10), 0);
        assert_eq!(wrapped_height("abc", 10), 1);
        assert_eq!(wrapped_height("abcdefghijk", 10), 2);
        assert_eq!(wrapped_height("a\n\nb", 10), 3);
    }

    #[test]
    fn the_footer_shows_the_config_note_when_there_is_no_status() {
        let mut app = app(1);
        app.config_note = Some("no configuration file".to_string());
        assert!(render(&app, 110, 20).contains("no configuration file"));
    }

    #[test]
    fn a_tiny_terminal_does_not_panic() {
        let app = app(2);
        let _ = render(&app, 20, 5);
        let _ = render(&app, 1, 1);
    }
}
