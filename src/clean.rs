//! The `clean` and `check-config` commands: glue between the config, the inventory, the rule
//! engine and the executor.

use crate::collect::collect_inventory;
use crate::config::Config;
use crate::elevate::{Elevator, WindowsElevator, execute_with_elevation, is_elevated};
use crate::executor::{has_failures, render_report};
use crate::plan::render_plan;
use crate::protect::Protection;
use crate::rules::build_plan;
use crate::winproc::WindowsControl;
use anyhow::Result;
use std::io::{BufRead, Write};
use std::path::Path;
use std::time::Duration;

pub struct CleanOptions<'a> {
    pub config_path: &'a Path,
    pub profile: Option<&'a str>,
    /// Show the plan and stop: nothing is touched.
    pub dry_run: bool,
    /// Skip the confirmation prompt (for scheduled tasks and scripts).
    pub assume_yes: bool,
    /// Allow relaunching as administrator (one UAC prompt) for processes that deny termination.
    pub allow_elevation: bool,
}

/// Asks a yes/no question. Only "y" or "yes" (any case) means yes; anything else, including an
/// empty line or the end of the input, means no.
pub fn confirm(
    question: &str,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> std::io::Result<bool> {
    write!(output, "{question} [y/N] ")?;
    output.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    let answer = line.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}

/// One-paragraph summary printed by `check-config`.
pub fn summarize(config: &Config, path: &Path) -> String {
    let mut out = format!(
        "OK: {} is valid ({} rule(s), {} profile(s)).\n",
        path.display(),
        config.rule.len(),
        config.profiles.len()
    );
    for warning in config.warnings() {
        out.push_str(&format!("Warning: {warning}\n"));
    }
    out
}

pub fn run_check_config(path: &Path) -> Result<()> {
    let config = Config::load(path)?;
    print!("{}", summarize(&config, path));
    Ok(())
}

/// Runs a cleanup. Returns true when nothing failed (no denied or failed action).
pub fn run_clean(options: &CleanOptions) -> Result<bool> {
    let config = Config::load(options.config_path)?;
    let inventory = collect_inventory()?;
    let protection = Protection::new(&config.protect.windows, std::process::id());
    let plan = build_plan(&inventory, &config, options.profile, &protection)?;
    print!("{}", render_plan(&plan));
    if plan.actions.is_empty() {
        return Ok(true);
    }
    if options.dry_run {
        println!("\nDry run: nothing was changed.");
        return Ok(true);
    }
    if !options.assume_yes {
        println!();
        let stdin = std::io::stdin();
        let proceed = confirm(
            "Terminate these processes?",
            &mut stdin.lock(),
            &mut std::io::stdout(),
        )?;
        if !proceed {
            println!("Cancelled: nothing was changed.");
            return Ok(true);
        }
    }

    // Processes that deny termination are retried in ONE elevated batch (a single UAC prompt),
    // unless elevation is disabled or this process already runs as administrator.
    let elevator: Option<&dyn Elevator> =
        (options.allow_elevation && !is_elevated()).then_some(&WindowsElevator);
    let results = execute_with_elevation(
        &plan,
        &WindowsControl,
        elevator,
        &config.protect.windows,
        config.settings.grace_ms,
    );

    // Give the driver a moment to release the memory, then measure what was actually freed.
    std::thread::sleep(Duration::from_millis(500));
    let used =
        |inv: &crate::inventory::Inventory| inv.gpus.iter().map(|g| g.used_bytes).sum::<u64>();
    let freed = collect_inventory()
        .ok()
        .map(|after| used(&inventory).saturating_sub(used(&after)));
    println!();
    print!(
        "{}",
        render_report(&results, plan.total_reclaimable(), freed)
    );
    Ok(!has_failures(&results))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::path::PathBuf;

    fn ask(answer: &str) -> bool {
        let mut input = Cursor::new(answer.as_bytes().to_vec());
        let mut output = Vec::new();
        confirm("Proceed?", &mut input, &mut output).unwrap()
    }

    #[test]
    fn only_yes_answers_confirm() {
        assert!(ask("y\n"));
        assert!(ask("Y\n"));
        assert!(ask("yes\n"));
        assert!(ask("  YES  \n"));
        assert!(!ask("n\n"));
        assert!(!ask("\n"));
        assert!(!ask(""));
        assert!(!ask("maybe\n"));
    }

    #[test]
    fn the_prompt_is_written_to_the_output() {
        let mut input = Cursor::new(b"n\n".to_vec());
        let mut output = Vec::new();
        confirm("Proceed?", &mut input, &mut output).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "Proceed? [y/N] ");
    }

    #[test]
    fn the_summary_counts_rules_and_profiles_and_lists_warnings() {
        let text = r#"
[[rule]]
name = "w"
platform = "wsl"
distro = "Ubuntu"
match = { name = "python3" }
action = "kill"

[profiles]
p = ["w"]
"#;
        let config = Config::parse(text).unwrap();
        let summary = summarize(&config, &PathBuf::from("c.toml"));
        assert!(summary.contains("OK: c.toml is valid (1 rule(s), 1 profile(s))."));
        assert!(summary.contains("Warning: rule 'w' targets WSL"));
    }
}
