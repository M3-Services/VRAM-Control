use anyhow::Result;
use clap::{Parser, Subcommand};
use vramctl::collect::collect_inventory;
use vramctl::gpu::enumerate_gpus;
use vramctl::render::{render_gpus, render_table};

const MIB: u64 = 1024 * 1024;

/// Inventory GPU memory consumers.
#[derive(Parser)]
#[command(version, arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
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
}

fn main() -> Result<()> {
    match Cli::parse().command {
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
    }
    Ok(())
}
