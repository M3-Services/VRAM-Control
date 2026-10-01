//! The rule engine: turns an inventory and a configuration into a cleanup plan.
//! It only reads; it never acts on any process.

use crate::config::{ActionKind, Config, GpuFilter, MatchSpec, Platform, Rule};
use crate::inventory::{Inventory, ProcessEntry};
use crate::plan::{Plan, PlannedAction, ProcessAction, ProtectedHit};
use crate::protect::Protection;
use anyhow::{Result, bail};
use regex::{Regex, RegexBuilder};
use std::collections::HashSet;

const MIB: u64 = 1024 * 1024;

struct CompiledRule<'a> {
    rule: &'a Rule,
    action: ProcessAction,
    regex: Option<Regex>,
}

fn spec_matches(spec: &MatchSpec, regex: Option<&Regex>, process: &ProcessEntry) -> bool {
    spec.name
        .as_ref()
        .is_none_or(|n| process.name.eq_ignore_ascii_case(n))
        && regex.is_none_or(|r| r.is_match(&process.name))
        && spec
            .cmdline_contains
            .as_ref()
            .is_none_or(|c| process.cmdline.to_lowercase().contains(&c.to_lowercase()))
}

fn gpu_matches(filter: &GpuFilter, process: &ProcessEntry, inventory: &Inventory) -> bool {
    process.usage.iter().any(|usage| {
        inventory
            .gpus
            .iter()
            .filter(|summary| summary.gpu.luid == usage.luid)
            .any(|summary| match filter {
                GpuFilter::Index(i) => summary.gpu.index == *i,
                GpuFilter::Name(n) => summary.gpu.name.to_lowercase().contains(&n.to_lowercase()),
            })
    })
}

/// Builds the plan. Rules are tried in file order and the first matching rule wins for a process.
///
/// - With `profile = Some(name)` only the rules listed in that profile are used; otherwise all rules.
/// - Processes using less dedicated VRAM than `settings.min_vram_mb` are ignored.
/// - A process selected by a rule but covered by the protection list is reported, never planned.
/// - WSL rules are not supported yet: they only add a note.
pub fn build_plan(
    inventory: &Inventory,
    config: &Config,
    profile: Option<&str>,
    protection: &Protection,
) -> Result<Plan> {
    let enabled: Option<HashSet<&str>> = match profile {
        None => None,
        Some(name) => match config.profiles.get(name) {
            Some(rules) => Some(rules.iter().map(String::as_str).collect()),
            None => {
                let mut known: Vec<&str> = config.profiles.keys().map(String::as_str).collect();
                known.sort();
                bail!("unknown profile '{name}' (available: {})", known.join(", "));
            }
        },
    };

    let mut plan = Plan::default();
    let mut compiled = Vec::new();
    for rule in &config.rule {
        if enabled
            .as_ref()
            .is_some_and(|set| !set.contains(rule.name.as_str()))
        {
            continue;
        }
        if rule.platform == Platform::Wsl {
            plan.notes.push(format!(
                "rule '{}' targets WSL, which is not supported yet: ignored",
                rule.name
            ));
            continue;
        }
        let action = match rule.action {
            ActionKind::Terminate => ProcessAction::Terminate,
            ActionKind::Kill => ProcessAction::Kill,
            ActionKind::WslShutdown => continue,
        };
        let regex = rule
            .matcher
            .name_regex
            .as_ref()
            .map(|p| RegexBuilder::new(p).case_insensitive(true).build())
            .transpose()?;
        compiled.push(CompiledRule {
            rule,
            action,
            regex,
        });
    }

    let threshold = config.settings.min_vram_mb * MIB;
    for process in &inventory.processes {
        if process.dedicated_bytes() < threshold {
            continue;
        }
        let Some(hit) = compiled.iter().find(|c| {
            spec_matches(&c.rule.matcher, c.regex.as_ref(), process)
                && c.rule
                    .gpu
                    .as_ref()
                    .is_none_or(|f| gpu_matches(f, process, inventory))
        }) else {
            continue;
        };
        if protection.is_protected(process.pid, &process.name) {
            plan.protected.push(ProtectedHit {
                pid: process.pid,
                name: process.name.clone(),
                rule: hit.rule.name.clone(),
            });
            continue;
        }
        plan.actions.push(PlannedAction {
            pid: process.pid,
            name: process.name.clone(),
            start_time: process.start_time,
            rule: hit.rule.name.clone(),
            action: hit.action,
            tree: hit.rule.tree,
            reclaimable_bytes: process
                .usage
                .iter()
                .filter(|u| !u.suspect)
                .map(|u| u.dedicated_bytes)
                .sum(),
        });
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::build_inventory;
    use crate::model::{GpuInfo, Luid, ProcessMeta, ProcessSample};
    use std::collections::HashMap;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn gpu(index: usize, low: u32, name: &str) -> GpuInfo {
        GpuInfo {
            index,
            name: name.to_string(),
            luid: Luid::new(0, low),
            total_bytes: 24 * GIB,
        }
    }

    /// (pid, name, cmdline, gpu luid low part, dedicated bytes)
    type Spec<'a> = (u32, &'a str, &'a str, u32, u64);

    fn inventory(gpus: &[GpuInfo], processes: &[Spec]) -> Inventory {
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
                        start_time: u64::from(*pid) * 10,
                    },
                )
            })
            .collect();
        build_inventory(gpus, &[], &samples, &metas)
    }

    fn config(extra: &str) -> Config {
        Config::parse(extra).unwrap()
    }

    fn protection() -> Protection {
        Protection::new(&[], 99_999)
    }

    const ONE_RULE: &str = r#"
[[rule]]
name = "llama"
platform = "windows"
match = { name = "llama-server.exe" }
action = "terminate"
"#;

    #[test]
    fn selects_matching_processes_and_records_start_time_and_vram() {
        let inv = inventory(
            &[gpu(0, 1, "GPU A")],
            &[
                (10, "llama-server.exe", "", 1, 6 * GIB),
                (11, "other.exe", "", 1, 2 * GIB),
            ],
        );
        let plan = build_plan(&inv, &config(ONE_RULE), None, &protection()).unwrap();
        assert_eq!(plan.actions.len(), 1);
        let a = &plan.actions[0];
        assert_eq!(
            (a.pid, a.start_time, a.reclaimable_bytes),
            (10, 100, 6 * GIB)
        );
        assert_eq!(a.rule, "llama");
        assert_eq!(a.action, ProcessAction::Terminate);
    }

    #[test]
    fn matching_is_case_insensitive_and_criteria_are_combined_with_and() {
        let text = r#"
[[rule]]
name = "comfy"
platform = "windows"
match = { name = "PYTHON.EXE", cmdline_contains = "ComfyUI" }
action = "kill"
"#;
        let inv = inventory(
            &[gpu(0, 1, "GPU A")],
            &[
                (10, "python.exe", "python C:\\x\\comfyui\\main.py", 1, GIB),
                (11, "python.exe", "python other.py", 1, GIB),
            ],
        );
        let plan = build_plan(&inv, &config(text), None, &protection()).unwrap();
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].pid, 10);
        assert_eq!(plan.actions[0].action, ProcessAction::Kill);
    }

    #[test]
    fn name_regex_matches_the_process_name() {
        let text = r#"
[[rule]]
name = "ollama"
platform = "windows"
match = { name_regex = "^ollama" }
action = "terminate"
"#;
        let inv = inventory(
            &[gpu(0, 1, "GPU A")],
            &[
                (10, "ollama.exe", "", 1, GIB),
                (11, "ollama app.exe", "", 1, GIB),
                (12, "x.exe", "", 1, GIB),
            ],
        );
        let plan = build_plan(&inv, &config(text), None, &protection()).unwrap();
        let pids: Vec<u32> = plan.actions.iter().map(|a| a.pid).collect();
        assert_eq!(pids, vec![10, 11]);
    }

    #[test]
    fn protected_processes_are_reported_and_never_planned() {
        let text = r#"
[[rule]]
name = "everything python and dwm"
platform = "windows"
match = { name_regex = "dwm|python" }
action = "kill"
"#;
        let inv = inventory(
            &[gpu(0, 1, "GPU A")],
            &[
                (500, "dwm.exe", "", 1, GIB),
                (501, "python.exe", "", 1, GIB),
            ],
        );
        let plan = build_plan(&inv, &config(text), None, &protection()).unwrap();
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].pid, 501);
        assert_eq!(plan.protected.len(), 1);
        assert_eq!(plan.protected[0].name, "dwm.exe");
    }

    #[test]
    fn the_first_matching_rule_wins() {
        let text = r#"
[[rule]]
name = "first"
platform = "windows"
match = { name = "a.exe" }
action = "terminate"

[[rule]]
name = "second"
platform = "windows"
match = { name = "a.exe" }
action = "kill"
"#;
        let inv = inventory(&[gpu(0, 1, "GPU A")], &[(10, "a.exe", "", 1, GIB)]);
        let plan = build_plan(&inv, &config(text), None, &protection()).unwrap();
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].rule, "first");
    }

    #[test]
    fn small_consumers_are_ignored_using_the_configured_threshold() {
        let text = format!("[settings]\nmin_vram_mb = 500\n{ONE_RULE}");
        let inv = inventory(
            &[gpu(0, 1, "GPU A")],
            &[(10, "llama-server.exe", "", 1, 100 * 1024 * 1024)],
        );
        assert!(
            build_plan(&inv, &config(&text), None, &protection())
                .unwrap()
                .actions
                .is_empty()
        );
        let low = ONE_RULE.to_string() + "\n[settings]\nmin_vram_mb = 50\n";
        // Settings placed after the rule table still belong to the top-level [settings] table.
        assert_eq!(
            build_plan(&inv, &config(&low), None, &protection())
                .unwrap()
                .actions
                .len(),
            1
        );
    }

    #[test]
    fn a_profile_limits_the_rules_and_an_unknown_profile_is_an_error() {
        let text = r#"
[[rule]]
name = "a rule"
platform = "windows"
match = { name = "a.exe" }
action = "terminate"

[[rule]]
name = "b rule"
platform = "windows"
match = { name = "b.exe" }
action = "terminate"

[profiles]
only-a = ["a rule"]
"#;
        let inv = inventory(
            &[gpu(0, 1, "GPU A")],
            &[(10, "a.exe", "", 1, GIB), (11, "b.exe", "", 1, GIB)],
        );
        let cfg = config(text);
        let plan = build_plan(&inv, &cfg, Some("only-a"), &protection()).unwrap();
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].pid, 10);
        let all = build_plan(&inv, &cfg, None, &protection()).unwrap();
        assert_eq!(all.actions.len(), 2);
        let err = build_plan(&inv, &cfg, Some("nope"), &protection()).unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown profile 'nope' (available: only-a)")
        );
    }

    #[test]
    fn the_gpu_filter_restricts_a_rule_to_processes_on_that_gpu() {
        let text = r#"
[[rule]]
name = "second gpu only"
platform = "windows"
match = { name_regex = ".*" }
action = "terminate"
gpu = 1
"#;
        let inv = inventory(
            &[gpu(0, 1, "First GPU"), gpu(1, 2, "Second GPU")],
            &[(10, "a.exe", "", 1, GIB), (11, "b.exe", "", 2, GIB)],
        );
        let plan = build_plan(&inv, &config(text), None, &protection()).unwrap();
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].pid, 11);

        let by_name = text.replace("gpu = 1", "gpu = \"first\"");
        let plan = build_plan(&inv, &config(&by_name), None, &protection()).unwrap();
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].pid, 10);
    }

    #[test]
    fn suspect_readings_do_not_count_as_reclaimable() {
        let inv = inventory(
            &[gpu(0, 1, "GPU A")],
            &[(10, "llama-server.exe", "", 1, 55 * GIB)],
        );
        let plan = build_plan(&inv, &config(ONE_RULE), None, &protection()).unwrap();
        assert_eq!(plan.actions[0].reclaimable_bytes, 0);
    }

    #[test]
    fn wsl_rules_are_ignored_with_a_note() {
        let text = r#"
[[rule]]
name = "wsl rule"
platform = "wsl"
distro = "Ubuntu"
match = { name = "python3" }
action = "terminate"
"#;
        let inv = inventory(&[gpu(0, 1, "GPU A")], &[(10, "python3", "", 1, GIB)]);
        let plan = build_plan(&inv, &config(text), None, &protection()).unwrap();
        assert!(plan.actions.is_empty());
        assert_eq!(plan.notes.len(), 1);
        assert!(plan.notes[0].contains("wsl rule"));
    }
}
