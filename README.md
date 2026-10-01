# VRAM-Control

A command-line tool with a terminal UI (TUI) to **find out what is using your GPU memory** and
**free it** by terminating the processes you choose.

> **Status: design phase.** No release yet. This README describes the planned behavior;
> see [ROADMAP.md](ROADMAP.md) for ideas beyond the first version.

## Why

On Windows, GPU drivers run in WDDM mode and `nvidia-smi` typically shows `N/A` for
per-process memory. After unloading an LLM, VRAM can stay heavily used and it is hard to tell
which process holds it. VRAM-Control reads the per-process GPU memory counters that Windows
exposes and puts them in one place, together with the means to clean up.

## Planned features

- **Inventory** of per-process VRAM (dedicated and shared), with process name, PID, command
  line and parent.
- **Unattributed memory line**: the part of used VRAM that no process accounts for
  (driver and system reservations).
- **Multi-GPU aware**: lists the GPUs at startup and adapts the display to their number.
- **WSL2 support**: shows GPU processes running inside a running WSL distribution and can
  terminate them. VRAM-Control never starts WSL.
- **Cleanup** by selecting processes in the TUI, or by applying rules from a TOML file.
- **On-demand elevation**: processes that need administrator rights are handled through a
  single UAC prompt for the whole batch.
- **Scriptable CLI** with JSON output.

## Safety principles

- VRAM-Control **never modifies the configuration of any other tool**. It may read such files,
  but it never writes to them.
- The only actions it performs are terminating a process by PID, a targeted `kill` inside a
  running WSL distribution, and `wsl --shutdown` **only** when a rule names it explicitly.
- A built-in protection list (for example `dwm.exe`, `csrss.exe`, `explorer.exe`) can never be
  terminated, whatever the rules say.
- Every cleanup shows a plan first and asks for confirmation before acting.

## Planned usage

```text
vramctl                          # open the TUI
vramctl list [--json]            # print the inventory
vramctl gpus                     # list detected GPUs
vramctl clean [--profile <name>] [--dry-run] [--yes] [--no-elevate]
vramctl check-config             # validate the configuration file
```

A scheduled cleanup does not need a dedicated feature: run
`vramctl clean --profile <name> --yes` from a Windows scheduled task.

## Configuration

A single TOML file owned by VRAM-Control (default location:
`%APPDATA%\vramctl\vramctl.toml`, overridable with `--config`). Example:

```toml
[protect]
windows = ["obsidian.exe"]
wsl     = []

[settings]
refresh_ms   = 1000
grace_ms     = 3000
wsl_alert_mb = 500
min_vram_mb  = 100

[[rule]]
name     = "llama-server"
platform = "windows"
match    = { name = "llama-server.exe" }
action   = "terminate"

[[rule]]
name     = "python GPU jobs in WSL"
platform = "wsl"
distro   = "Ubuntu"
match    = { name = "python3" }
action   = "terminate"

[profiles]
free-max = ["llama-server"]
```

## Requirements

- Windows 11
- An NVIDIA GPU (other vendors are not validated yet)
- Rust toolchain to build from source

## Building

```text
cargo build --release
```

Release binaries will be published on the GitHub Releases page.

## License

MIT, see [LICENSE](LICENSE).
