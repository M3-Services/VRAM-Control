# Roadmap

Ideas that are intentionally **out of scope for v1**. Nothing here is a promise;
it is a place to park ideas so they are not lost.

Hard rule that applies to every idea below: VRAM-Control may *read* other tools'
configuration, but it never *modifies* it.

## Suggestions engine (read-only advice)

Read the configuration and command lines of other tools (LLM runtimes, ComfyUI,
etc.) and display **text-only hints**, for example:

- "This `llama-server` was started with a large context size."
- "This runtime keeps models loaded for N minutes after the last request."

Hints are never applied automatically.

## Background service with client mode

A small background service that keeps collecting VRAM data, with the TUI/CLI
connecting to it as a client. Possible benefits: history, alerts, no collection
cost at startup.

Not needed for scheduled cleanups: `vramctl clean --profile <name> --yes` can
already be run from a Windows scheduled task.

## History and alerts

- `watch` mode with a VRAM sparkline.
- Alert when VRAM usage crosses a threshold (for example 90%).
- "What changed since" diff, to spot what grew after unloading a model.

## Code signing

Sign release binaries so the UAC prompt shows a verified publisher instead of
"Unknown publisher".

## Other GPU vendors

Validate and document behavior on AMD and Intel GPUs. The Windows performance
counters are generic, but only NVIDIA is tested.
