//! The TOML configuration file owned by VRAM-Control.
//!
//! This file only describes VRAM-Control's own rules. The tool never writes to the configuration of
//! any other program.

use anyhow::{Context, Result, bail};
use regex::RegexBuilder;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Windows,
    Wsl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionKind {
    /// Graceful stop first (close request), then forced termination after the grace delay.
    Terminate,
    /// Forced termination right away.
    Kill,
    /// `wsl --shutdown`. Only valid for WSL rules and only runs when written explicitly.
    WslShutdown,
}

/// Restricts a rule to processes that use a given GPU, by index or by name fragment.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum GpuFilter {
    Index(usize),
    Name(String),
}

/// Criteria that must ALL match for a process to be selected by a rule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchSpec {
    /// Exact process name, case-insensitive.
    pub name: Option<String>,
    /// Regular expression applied to the process name, case-insensitive.
    pub name_regex: Option<String>,
    /// Case-insensitive fragment searched in the full command line.
    pub cmdline_contains: Option<String>,
}

impl MatchSpec {
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.name_regex.is_none() && self.cmdline_contains.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    pub platform: Platform,
    /// Required for WSL rules, forbidden for Windows rules.
    pub distro: Option<String>,
    #[serde(rename = "match", default)]
    pub matcher: MatchSpec,
    pub action: ActionKind,
    /// Also terminate the process's descendants (Windows only). Defaults to false.
    #[serde(default)]
    pub tree: bool,
    pub gpu: Option<GpuFilter>,
}

/// Processes that must never be terminated, in addition to the built-in protection list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Protect {
    #[serde(default)]
    pub windows: Vec<String>,
    #[serde(default)]
    pub wsl: Vec<String>,
}

/// Optional tuning. Every key has a documented default.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// TUI refresh interval, in milliseconds.
    pub refresh_ms: u64,
    /// Delay between the graceful stop request and forced termination, in milliseconds.
    pub grace_ms: u64,
    /// VRAM used by WSL above which the inventory flags it, in MiB.
    pub wsl_alert_mb: u64,
    /// Processes using less dedicated VRAM than this are ignored by cleanup plans, in MiB.
    pub min_vram_mb: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            refresh_ms: 1000,
            grace_ms: 3000,
            wsl_alert_mb: 500,
            min_vram_mb: 100,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub protect: Protect,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub rule: Vec<Rule>,
    /// Profile name -> names of the rules it enables.
    #[serde(default)]
    pub profiles: HashMap<String, Vec<String>>,
}

impl Config {
    /// Parses and validates a configuration. All validation errors are reported together.
    pub fn parse(text: &str) -> Result<Config> {
        let config: Config = toml::from_str(text).context("invalid configuration file")?;
        let errors = config.validation_errors();
        if !errors.is_empty() {
            let list: Vec<String> = errors.iter().map(|e| format!("- {e}")).collect();
            bail!("invalid configuration:\n{}", list.join("\n"));
        }
        Ok(config)
    }

    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read the configuration file {}", path.display()))?;
        Config::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    fn validation_errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.settings.refresh_ms == 0 {
            errors.push("settings.refresh_ms must be greater than 0".to_string());
        }
        let mut seen = HashSet::new();
        for rule in &self.rule {
            let label = format!("rule '{}'", rule.name);
            if rule.name.trim().is_empty() {
                errors.push("a rule has an empty name".to_string());
            } else if !seen.insert(rule.name.as_str()) {
                errors.push(format!("{label}: the name is used more than once"));
            }
            match (rule.platform, &rule.distro) {
                (Platform::Wsl, None) => {
                    errors.push(format!("{label}: 'distro' is required for WSL rules"))
                }
                (Platform::Windows, Some(_)) => {
                    errors.push(format!("{label}: 'distro' is only allowed for WSL rules"))
                }
                _ => {}
            }
            match rule.action {
                ActionKind::WslShutdown => {
                    if rule.platform != Platform::Wsl {
                        errors.push(format!(
                            "{label}: 'wsl-shutdown' requires platform = \"wsl\""
                        ));
                    }
                    if !rule.matcher.is_empty() {
                        errors.push(format!(
                            "{label}: 'wsl-shutdown' does not take a 'match' table"
                        ));
                    }
                }
                ActionKind::Terminate | ActionKind::Kill => {
                    if rule.matcher.is_empty() {
                        errors.push(format!(
                            "{label}: needs at least one 'match' criterion (an empty match would target every process)"
                        ));
                    }
                }
            }
            if let Some(pattern) = &rule.matcher.name_regex
                && let Err(e) = RegexBuilder::new(pattern).case_insensitive(true).build()
            {
                errors.push(format!("{label}: invalid name_regex: {e}"));
            }
        }
        let known: HashSet<&str> = self.rule.iter().map(|r| r.name.as_str()).collect();
        let mut profile_names: Vec<&String> = self.profiles.keys().collect();
        profile_names.sort();
        for profile in profile_names {
            for rule_name in &self.profiles[profile] {
                if !known.contains(rule_name.as_str()) {
                    errors.push(format!("profile '{profile}': unknown rule '{rule_name}'"));
                }
            }
        }
        errors
    }

    /// Non-fatal remarks about a valid configuration.
    pub fn warnings(&self) -> Vec<String> {
        self.rule
            .iter()
            .filter(|r| r.platform == Platform::Wsl)
            .map(|r| {
                format!(
                    "rule '{}' targets WSL, which is not supported yet: it is accepted but ignored",
                    r.name
                )
            })
            .collect()
    }
}

/// Default location: `%APPDATA%\vramctl\vramctl.toml`.
pub fn default_config_path() -> Result<PathBuf> {
    let appdata =
        std::env::var_os("APPDATA").context("the APPDATA environment variable is not set")?;
    Ok(PathBuf::from(appdata).join("vramctl").join("vramctl.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
[protect]
windows = ["obsidian.exe"]

[settings]
grace_ms = 1500
min_vram_mb = 50

[[rule]]
name = "llama"
platform = "windows"
match = { name = "llama-server.exe" }
action = "terminate"
tree = true

[[rule]]
name = "comfy"
platform = "windows"
match = { name = "python.exe", cmdline_contains = "comfyui" }
action = "kill"
gpu = 0

[[rule]]
name = "wsl all"
platform = "wsl"
distro = "Ubuntu"
action = "wsl-shutdown"

[profiles]
free = ["llama", "comfy"]
"#;

    fn errors_of(text: &str) -> String {
        format!("{:#}", Config::parse(text).unwrap_err())
    }

    #[test]
    fn parses_a_full_configuration() {
        let config = Config::parse(FULL).unwrap();
        assert_eq!(config.protect.windows, vec!["obsidian.exe"]);
        assert_eq!(config.settings.grace_ms, 1500);
        assert_eq!(config.settings.min_vram_mb, 50);
        assert_eq!(config.rule.len(), 3);
        assert_eq!(config.rule[0].action, ActionKind::Terminate);
        assert!(config.rule[0].tree);
        assert_eq!(config.rule[1].gpu, Some(GpuFilter::Index(0)));
        assert_eq!(config.rule[2].action, ActionKind::WslShutdown);
        assert_eq!(config.profiles["free"], vec!["llama", "comfy"]);
    }

    #[test]
    fn an_empty_file_gives_the_documented_defaults() {
        let config = Config::parse("").unwrap();
        assert_eq!(config.settings, Settings::default());
        assert_eq!(config.settings.grace_ms, 3000);
        assert!(config.rule.is_empty());
    }

    #[test]
    fn gpu_filter_accepts_a_name() {
        let text = r#"
[[rule]]
name = "r"
platform = "windows"
match = { name = "a.exe" }
action = "kill"
gpu = "4090"
"#;
        let config = Config::parse(text).unwrap();
        assert_eq!(
            config.rule[0].gpu,
            Some(GpuFilter::Name("4090".to_string()))
        );
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let text = r#"
[[rule]]
name = "r"
platform = "windows"
match = { name = "a.exe" }
action = "kill"
actoin = "typo"
"#;
        assert!(errors_of(text).contains("actoin"));
        assert!(errors_of("[settings]\ngrace = 1\n").contains("grace"));
    }

    #[test]
    fn missing_required_keys_are_rejected() {
        let text = r#"
[[rule]]
name = "r"
match = { name = "a.exe" }
action = "kill"
"#;
        assert!(errors_of(text).contains("platform"));
    }

    #[test]
    fn wsl_rules_need_a_distro_and_windows_rules_must_not_have_one() {
        let wsl = "[[rule]]\nname = \"r\"\nplatform = \"wsl\"\nmatch = { name = \"a\" }\naction = \"kill\"\n";
        assert!(errors_of(wsl).contains("'distro' is required"));
        let win = "[[rule]]\nname = \"r\"\nplatform = \"windows\"\ndistro = \"U\"\nmatch = { name = \"a\" }\naction = \"kill\"\n";
        assert!(errors_of(win).contains("only allowed for WSL"));
    }

    #[test]
    fn kill_rules_need_a_match_criterion() {
        let text = "[[rule]]\nname = \"r\"\nplatform = \"windows\"\naction = \"terminate\"\n";
        assert!(errors_of(text).contains("at least one 'match' criterion"));
    }

    #[test]
    fn wsl_shutdown_is_wsl_only_and_takes_no_match() {
        let win = "[[rule]]\nname = \"r\"\nplatform = \"windows\"\naction = \"wsl-shutdown\"\n";
        assert!(errors_of(win).contains("requires platform"));
        let with_match = "[[rule]]\nname = \"r\"\nplatform = \"wsl\"\ndistro = \"U\"\nmatch = { name = \"a\" }\naction = \"wsl-shutdown\"\n";
        assert!(errors_of(with_match).contains("does not take a 'match'"));
    }

    #[test]
    fn invalid_regex_duplicate_names_and_unknown_profile_rules_are_reported_together() {
        let text = r#"
[[rule]]
name = "r"
platform = "windows"
match = { name_regex = "(" }
action = "kill"

[[rule]]
name = "r"
platform = "windows"
match = { name = "a.exe" }
action = "kill"

[profiles]
p = ["ghost"]
"#;
        let message = errors_of(text);
        assert!(message.contains("invalid name_regex"));
        assert!(message.contains("used more than once"));
        assert!(message.contains("profile 'p': unknown rule 'ghost'"));
    }

    #[test]
    fn the_shipped_example_configuration_is_valid() {
        let config = Config::parse(include_str!("../vramctl.example.toml")).unwrap();
        assert_eq!(config.rule.len(), 2);
        assert_eq!(config.profiles["free-max"].len(), 2);
    }

    #[test]
    fn wsl_rules_produce_a_warning() {
        let config = Config::parse(FULL).unwrap();
        let warnings = config.warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("wsl all"));
    }
}
