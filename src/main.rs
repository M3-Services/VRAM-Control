use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use vramctl::clean::{CleanOptions, run_check_config, run_clean};
use vramctl::collect::collect_inventory;
use vramctl::config::default_config_path;
use vramctl::gpu::enumerate_gpus;
use vramctl::render::{render_gpus, render_table};
use vramctl::tui;

const MIB: u64 = 1024 * 1024;

/// Inventory GPU memory consumers and free VRAM by terminating selected processes.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Configuration file (default: %APPDATA%\vramctl\vramctl.toml).
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Without a command, the interactive terminal UI opens.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print the per-process GPU memory inventory.
    List {
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Hide processes using less than this many MiB of dedicated memory (table output only).
        #[arg(long, value_name = "MIB", default_value_t = 0)]
        min_mb: u64,
    },
    /// List the detected GPUs.
    Gpus {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Terminate the processes selected by the configured rules, after showing a plan.
    Clean {
        /// Only use the rules listed in this profile (default: all rules).
        #[arg(long)]
        profile: Option<String>,
        /// Show the plan and stop: nothing is touched.
        #[arg(long)]
        dry_run: bool,
        /// Do not ask for confirmation (for scheduled tasks and scripts).
        #[arg(long)]
        yes: bool,
    },
    /// Validate the configuration file without doing anything else.
    CheckConfig,
}

fn config_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    match explicit {
        Some(path) => Ok(path),
        None => default_config_path(),
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let Some(command) = cli.command else {
        return tui::run(&config_path(cli.config)?);
    };
    match command {
        Command::List { json, min_mb } => {
            let inventory = collect_inventory()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&inventory)?);
            } else {
                print!("{}", render_table(&inventory, min_mb * MIB));
            }
        }
        Command::Gpus { json } => {
            let gpus = enumerate_gpus()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&gpus)?);
            } else {
                print!("{}", render_gpus(&gpus));
            }
        }
        Command::Clean {
            profile,
            dry_run,
            yes,
        } => {
            let path = config_path(cli.config)?;
            let all_ok = run_clean(&CleanOptions {
                config_path: &path,
                profile: profile.as_deref(),
                dry_run,
                assume_yes: yes,
            })?;
            if !all_ok {
                std::process::exit(1);
            }
        }
        Command::CheckConfig => run_check_config(&config_path(cli.config)?)?,
    }
    Ok(())
}
