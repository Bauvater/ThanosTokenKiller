//! Layered configuration.
//!
//! Precedence (highest first):
//! `CLI argument` → `TTK_*` environment → session file → project file
//! (`<repo>/.ttk/config.toml` or `<repo>/ttk.toml`) → user file
//! (`<config dir>/ttk/config.toml`) → built-in defaults.
//!
//! Unknown keys are a hard error: a typo in a safety-relevant key must never
//! be silently ignored.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Analyse only, never change a single byte.
    Observe,
    /// Deterministic, validated, fully reversible transformations only.
    Safe,
    /// Adds query-aware selection and controlled compression.
    Balanced,
    /// Most aggressive settings the quality firewall still allows.
    Maximum,
    /// Like safe, but keeps errors and stack traces more generously.
    Debug,
    /// Maximum provenance, minimal semantic change.
    Forensic,
    /// Refuses anything that would need a network call.
    Offline,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Observe => "observe",
            Mode::Safe => "safe",
            Mode::Balanced => "balanced",
            Mode::Maximum => "maximum",
            Mode::Debug => "debug",
            Mode::Forensic => "forensic",
            Mode::Offline => "offline",
        }
    }

    /// Observe mode is the only mode that guarantees byte-identical output.
    pub fn transforms(self) -> bool {
        !matches!(self, Mode::Observe)
    }

    /// Whether lossy-but-reversible summarisation of low-risk content is
    /// allowed. Never true in safe/observe/forensic.
    pub fn allows_lossy(self) -> bool {
        matches!(self, Mode::Balanced | Mode::Maximum)
    }
}

impl std::str::FromStr for Mode {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "observe" => Mode::Observe,
            "safe" => Mode::Safe,
            "balanced" => Mode::Balanced,
            "maximum" | "max" => Mode::Maximum,
            "debug" => Mode::Debug,
            "forensic" => Mode::Forensic,
            "offline" => Mode::Offline,
            other => {
                return Err(Error::Config(format!(
                    "unknown mode `{other}` (expected one of: observe, safe, balanced, maximum, debug, forensic, offline)"
                )));
            }
        })
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputStyle {
    Normal,
    Concise,
    Dense,
    PatchOnly,
    Machine,
    Forensic,
}

impl std::str::FromStr for OutputStyle {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(
            match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
                "normal" => OutputStyle::Normal,
                "concise" => OutputStyle::Concise,
                "dense" => OutputStyle::Dense,
                "patch-only" => OutputStyle::PatchOnly,
                "machine" => OutputStyle::Machine,
                "forensic" => OutputStyle::Forensic,
                other => return Err(Error::Config(format!("unknown output_style `{other}`"))),
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub mode: Mode,
    pub output_style: OutputStyle,
    pub capsules: CapsuleConfig,
    pub quality: QualityConfig,
    pub context: ContextConfig,
    pub security: SecurityConfig,
    pub telemetry: TelemetryConfig,
    pub limits: LimitsConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CapsuleConfig {
    pub raw_ttl_days: u32,
    pub max_storage_gb: f64,
    /// `zstd` or `none`.
    pub compression: String,
    pub compression_level: i32,
    pub encryption: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct QualityConfig {
    pub preserve_line_numbers: bool,
    pub preserve_commands: bool,
    pub preserve_urls: bool,
    pub allow_semantic_code_compression: bool,
    pub allow_abstractive_summaries: bool,
    /// A transformation must save at least this fraction to be kept.
    pub min_relative_gain: f64,
}

/// Reserved for the context compiler.
///
/// These keys are parsed and validated so a configuration written today stays
/// valid, but **nothing reads them yet** — there is no context compiler. See
/// `docs/roadmap.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ContextConfig {
    pub retrieval_reserve_percent: u32,
    pub minimum_output_reserve_tokens: u32,
    pub deduplicate: bool,
    pub delta_context: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SecurityConfig {
    pub redact_secrets: bool,
    pub redact_pii: bool,
    pub block_secret_exfiltration: bool,
    /// Refuse `ttk raw` for capsules marked secret unless explicitly confirmed.
    pub gate_raw_access_for_secrets: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TelemetryConfig {
    /// Local JSONL event log. Never leaves the machine.
    pub enabled: bool,
    /// Sending telemetry anywhere. Always opt-in, unimplemented for now.
    pub remote: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsConfig {
    /// Content larger than this is stored and referenced but never parsed.
    pub max_parse_bytes: u64,
    /// Maximum nesting depth accepted by structural parsers.
    pub max_parse_depth: u32,
    /// Upper bound on a single captured stream.
    pub max_capture_bytes: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // Safe is the default until benchmarks justify balanced.
            mode: Mode::Safe,
            output_style: OutputStyle::Dense,
            capsules: CapsuleConfig::default(),
            quality: QualityConfig::default(),
            context: ContextConfig::default(),
            security: SecurityConfig::default(),
            telemetry: TelemetryConfig::default(),
            limits: LimitsConfig::default(),
        }
    }
}

impl Default for CapsuleConfig {
    fn default() -> Self {
        Self {
            raw_ttl_days: 14,
            max_storage_gb: 20.0,
            compression: "zstd".to_string(),
            compression_level: 6,
            encryption: false,
        }
    }
}

impl Default for QualityConfig {
    fn default() -> Self {
        Self {
            preserve_line_numbers: true,
            preserve_commands: true,
            preserve_urls: true,
            allow_semantic_code_compression: false,
            allow_abstractive_summaries: false,
            min_relative_gain: 0.05,
        }
    }
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            retrieval_reserve_percent: 10,
            minimum_output_reserve_tokens: 2048,
            deduplicate: true,
            delta_context: true,
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            redact_secrets: true,
            redact_pii: false,
            block_secret_exfiltration: true,
            gate_raw_access_for_secrets: true,
        }
    }
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            remote: false,
        }
    }
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_parse_bytes: 32 * 1024 * 1024,
            max_parse_depth: 128,
            max_capture_bytes: 256 * 1024 * 1024,
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.context.retrieval_reserve_percent > 90 {
            return Err(Error::Config(
                "context.retrieval_reserve_percent must be <= 90".into(),
            ));
        }
        if !matches!(self.capsules.compression.as_str(), "zstd" | "none") {
            return Err(Error::Config(format!(
                "capsules.compression must be `zstd` or `none`, got `{}`",
                self.capsules.compression
            )));
        }
        if !(1..=22).contains(&self.capsules.compression_level) {
            return Err(Error::Config(
                "capsules.compression_level must be between 1 and 22".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.quality.min_relative_gain) {
            return Err(Error::Config(
                "quality.min_relative_gain must be between 0.0 and 1.0".into(),
            ));
        }
        if self.capsules.encryption {
            return Err(Error::Config(
                "capsules.encryption is not implemented yet; leave it false rather than \
                 believing capsules are encrypted"
                    .into(),
            ));
        }
        if self.telemetry.remote {
            return Err(Error::Config(
                "telemetry.remote is not implemented; ThanosTokenKiller never sends data off-machine"
                    .into(),
            ));
        }
        Ok(())
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text).map_err(|e| Error::Config(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("config is serializable")
    }
}

/// Where a configuration value came from. Reported by `ttk config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigOrigin {
    Default,
    UserFile(PathBuf),
    ProjectFile(PathBuf),
    SessionFile(PathBuf),
    Environment,
    CliArgument,
}

impl fmt::Display for ConfigOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigOrigin::Default => f.write_str("default"),
            ConfigOrigin::UserFile(p) => write!(f, "user file ({})", p.display()),
            ConfigOrigin::ProjectFile(p) => write!(f, "project file ({})", p.display()),
            ConfigOrigin::SessionFile(p) => write!(f, "session file ({})", p.display()),
            ConfigOrigin::Environment => f.write_str("environment"),
            ConfigOrigin::CliArgument => f.write_str("cli argument"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: Config,
    /// Every layer that contributed, in application order.
    pub layers: Vec<ConfigOrigin>,
    /// Repository / project root that was detected.
    pub project_root: Option<PathBuf>,
}

/// User level config file path (`%APPDATA%/ttk/config.toml`,
/// `~/.config/ttk/config.toml`).
pub fn user_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("ttk").join("config.toml"))
}

/// Walk upwards from `start` looking for a project marker.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        for marker in [
            ".ttk",
            ".git",
            "Cargo.toml",
            "package.json",
            "pyproject.toml",
        ] {
            if dir.join(marker).exists() {
                return Some(dir.to_path_buf());
            }
        }
        cur = dir.parent();
    }
    None
}

fn project_config_path(root: &Path) -> Option<PathBuf> {
    [root.join(".ttk").join("config.toml"), root.join("ttk.toml")]
        .into_iter()
        .find(|candidate| candidate.is_file())
}

/// Load the effective configuration for `cwd`.
///
/// Layers that do not exist are skipped silently; layers that exist but are
/// invalid are a hard error naming the file.
pub fn load(cwd: &Path) -> Result<LoadedConfig> {
    let mut layers = vec![ConfigOrigin::Default];
    let mut merged = toml::Table::new();

    if let Some(p) = user_config_path()
        && p.is_file()
    {
        merge_file(&mut merged, &p)?;
        layers.push(ConfigOrigin::UserFile(p));
    }

    let project_root = find_project_root(cwd);
    if let Some(root) = &project_root
        && let Some(p) = project_config_path(root)
    {
        merge_file(&mut merged, &p)?;
        layers.push(ConfigOrigin::ProjectFile(p));
    }

    if let Ok(p) = std::env::var("TTK_SESSION_CONFIG") {
        let path = PathBuf::from(p);
        if path.is_file() {
            merge_file(&mut merged, &path)?;
            layers.push(ConfigOrigin::SessionFile(path));
        }
    }

    if apply_env(&mut merged)? {
        layers.push(ConfigOrigin::Environment);
    }

    let config: Config = merged
        .try_into()
        .map_err(|e| Error::Config(format!("invalid configuration: {e}")))?;
    config.validate()?;

    Ok(LoadedConfig {
        config,
        layers,
        project_root,
    })
}

fn merge_file(target: &mut toml::Table, path: &Path) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
    let table: toml::Table = toml::from_str(&text)
        .map_err(|e| Error::Config(format!("invalid TOML in {}: {e}", path.display())))?;
    merge_tables(target, table);
    Ok(())
}

fn merge_tables(target: &mut toml::Table, source: toml::Table) {
    for (k, v) in source {
        match (target.get_mut(&k), v) {
            (Some(toml::Value::Table(existing)), toml::Value::Table(new)) => {
                merge_tables(existing, new);
            }
            (_, v) => {
                target.insert(k, v);
            }
        }
    }
}

/// `TTK_MODE`, `TTK_OUTPUT_STYLE`, `TTK_TELEMETRY` are the supported
/// environment overrides. Everything else stays file-driven on purpose.
fn apply_env(target: &mut toml::Table) -> Result<bool> {
    let mut touched = false;
    if let Ok(v) = std::env::var("TTK_MODE") {
        let mode: Mode = v.parse()?;
        target.insert("mode".into(), toml::Value::String(mode.as_str().into()));
        touched = true;
    }
    if let Ok(v) = std::env::var("TTK_OUTPUT_STYLE") {
        let style: OutputStyle = v.parse()?;
        let s = serde_json::to_value(style).map_err(|e| Error::Config(e.to_string()))?;
        target.insert(
            "output_style".into(),
            toml::Value::String(s.as_str().unwrap_or("dense").to_string()),
        );
        touched = true;
    }
    if let Ok(v) = std::env::var("TTK_TELEMETRY") {
        let enabled = matches!(v.trim(), "1" | "true" | "on" | "yes");
        let entry = target
            .entry("telemetry".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(t) = entry {
            t.insert("enabled".into(), toml::Value::Boolean(enabled));
        }
        touched = true;
    }
    Ok(touched)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe() {
        let c = Config::default();
        assert_eq!(c.mode, Mode::Safe);
        assert!(!c.quality.allow_semantic_code_compression);
        assert!(!c.quality.allow_abstractive_summaries);
        assert!(c.security.redact_secrets);
        assert!(!c.telemetry.remote);
        c.validate().expect("defaults must validate");
    }

    #[test]
    fn toml_roundtrip() {
        let c = Config::default();
        let back = Config::from_toml(&c.to_toml()).expect("roundtrip");
        assert_eq!(c, back);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = Config::from_toml("mdoe = \"safe\"").expect_err("typo must fail");
        assert!(err.to_string().contains("mdoe"), "{err}");
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(Config::from_toml("[capsules]\ncompression = \"lzma\"").is_err());
        assert!(Config::from_toml("[context]\nretrieval_reserve_percent = 95").is_err());
        assert!(Config::from_toml("[telemetry]\nremote = true").is_err());
        // Features that do not exist yet must not be silently accepted.
        assert!(Config::from_toml("[capsules]\nencryption = true").is_err());
    }

    #[test]
    fn partial_config_keeps_defaults() {
        let c = Config::from_toml("mode = \"observe\"").expect("parse");
        assert_eq!(c.mode, Mode::Observe);
        assert_eq!(
            c.capsules.raw_ttl_days,
            CapsuleConfig::default().raw_ttl_days
        );
    }

    #[test]
    fn mode_parsing() {
        assert_eq!("BALANCED".parse::<Mode>().unwrap(), Mode::Balanced);
        assert_eq!("max".parse::<Mode>().unwrap(), Mode::Maximum);
        assert!("turbo".parse::<Mode>().is_err());
        assert!(!Mode::Observe.transforms());
        assert!(!Mode::Safe.allows_lossy());
        assert!(Mode::Balanced.allows_lossy());
    }

    #[test]
    fn tables_merge_recursively() {
        let mut a: toml::Table =
            toml::from_str("[capsules]\nraw_ttl_days = 3\nencryption = true").expect("parse a");
        let b: toml::Table = toml::from_str("[capsules]\nraw_ttl_days = 9").expect("parse b");
        merge_tables(&mut a, b);
        let c: Config = a.try_into().expect("merged config");
        assert_eq!(c.capsules.raw_ttl_days, 9);
        assert!(c.capsules.encryption);
    }
}
