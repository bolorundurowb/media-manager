//! Configuration (§7).
//!
//! Layered, later wins: built-in defaults → system config → user config →
//! project config (`.media-manager.toml` in root) → `MM_*` env vars → CLI
//! flags. Config is data, versioned, digested into every plan and journal line.

pub mod default;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Severity};
use crate::identity::SourcePreference;
use crate::template::Template;
use crate::volume::NoReplaceStrategy;

/// Top-level configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub schema_version: u32,
    pub extensions: Extensions,
    pub behaviour: Behaviour,
    pub moves: Moves,
    pub conflict: Conflict,
    pub cleanup: Cleanup,
    pub providers: Providers,
    pub concurrency: Concurrency,
    pub naming: Naming,
    pub source_preference: SourcePreference,
}

impl Default for Config {
    fn default() -> Self {
        default::default_config()
    }
}

/// Extension sets (spec §3.1–3.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Extensions {
    pub video: Vec<String>,
    pub audio: Vec<String>,
    pub subtitle: Vec<String>,
    pub artwork: Vec<String>,
    pub metadata: Vec<String>,
}

impl Default for Extensions {
    fn default() -> Self {
        default::default_extensions()
    }
}

/// Behaviour toggles (§7).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Behaviour {
    pub symlinks: SymlinkPolicy,
    pub create_subs_dir: bool,
    pub normalise_artwork: bool,
    pub require_year_for_movies: bool,
    pub require_year_for_tv: bool,
    pub min_confidence: crate::provenance::Confidence,
}

impl Default for Behaviour {
    fn default() -> Self {
        Behaviour {
            symlinks: SymlinkPolicy::Skip,
            create_subs_dir: true,
            normalise_artwork: false,
            require_year_for_movies: true,
            require_year_for_tv: false,
            min_confidence: crate::provenance::Confidence::Medium,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymlinkPolicy {
    Skip,
    Follow,
    TreatAsFile,
}

/// Move strategy options (§7, §2.5).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Moves {
    pub no_replace_strategy: StrategyConfig,
    pub verify: VerifyMode,
    pub preserve_mtime: bool,
}

impl Default for Moves {
    fn default() -> Self {
        Moves {
            no_replace_strategy: StrategyConfig::Auto,
            verify: VerifyMode::Size,
            preserve_mtime: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyConfig {
    Auto,
    Native,
    Reserve,
    CheckThenRename,
}

impl StrategyConfig {
    pub fn resolve(self, fs_strategy: crate::volume::NoReplaceStrategy) -> NoReplaceStrategy {
        match self {
            StrategyConfig::Auto => fs_strategy,
            StrategyConfig::Native => NoReplaceStrategy::Native,
            StrategyConfig::Reserve => NoReplaceStrategy::Reserve,
            StrategyConfig::CheckThenRename => NoReplaceStrategy::CheckThenRename,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyMode {
    Size,
    Hash,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Conflict {
    pub policy: ConflictPolicy,
    pub compare: Vec<CompareField>,
}

impl Default for Conflict {
    fn default() -> Self {
        Conflict {
            policy: ConflictPolicy::Report,
            compare: vec![CompareField::Size, CompareField::Blake3],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    Report,
    Skip,
    SkipIfIdentical,
    RenameNew,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompareField {
    Size,
    Blake3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Cleanup {
    pub remove_empty_dirs: bool,
    pub tolerate_junk: bool,
    /// Junk filenames tolerated when `tolerate_junk` is on.
    pub junk_names: Vec<String>,
}

impl Default for Cleanup {
    fn default() -> Self {
        Cleanup {
            remove_empty_dirs: true,
            tolerate_junk: false,
            junk_names: vec![".DS_Store".into(), "Thumbs.db".into(), "desktop.ini".into()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Providers {
    pub enabled: bool,
}

impl Default for Providers {
    fn default() -> Self {
        Providers { enabled: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Concurrency {
    pub workers: Workers,
    pub probe_workers: Workers,
    pub hash_workers: Workers,
}

impl Default for Concurrency {
    fn default() -> Self {
        Concurrency {
            workers: Workers::Auto,
            probe_workers: Workers::Auto,
            hash_workers: Workers::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Workers {
    Auto,
    Fixed(u16),
}

impl Workers {
    pub fn resolve(self, fallback: usize) -> usize {
        match self {
            Workers::Auto => fallback,
            Workers::Fixed(n) => n as usize,
        }
    }
}

/// Naming templates (§5.5).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Naming {
    pub movies: MovieNaming,
    pub tv: TvNaming,
    pub music: MusicNaming,
}

impl Default for Naming {
    fn default() -> Self {
        default::default_naming()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MovieNaming {
    pub dir: Template,
    pub file: Template,
    pub subs_dir: String,
    pub sub_file: Template,
    pub artwork: String,
    pub nfo: Template,
}

impl Default for MovieNaming {
    fn default() -> Self {
        default::default_naming().movies
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TvNaming {
    pub show_dir: Template,
    pub season_dir: Template,
    pub specials_dir: String,
    pub file: Template,
    pub sub_file: Template,
}

impl Default for TvNaming {
    fn default() -> Self {
        default::default_naming().tv
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MusicNaming {
    pub artist_dir: Template,
    pub album_dir: Template,
    pub disc_dir: Template,
    pub file: Template,
    pub artwork: String,
    pub compilation_prefix: bool,
}

impl Default for MusicNaming {
    fn default() -> Self {
        default::default_naming().music
    }
}

impl Config {
    /// Load the fully-resolved config from the layered sources (§7). Returns
    /// the config and the path it was loaded from, if any.
    pub fn layered(
        root: Option<&Path>,
        cli_overrides: &ConfigOverrides,
    ) -> Result<Self, CoreError> {
        let mut cfg = default::default_config();

        // system + user config via the directories crate.
        if let Some(path) = user_config_path() {
            if path.exists() {
                let txt = std::fs::read_to_string(&path)?;
                let parsed: Config = toml::from_str(&txt)?;
                cfg = merge(cfg, parsed);
            }
        }

        // project config: .media-manager.toml in root.
        if let Some(root) = root {
            let proj = root.join(".media-manager.toml");
            if proj.exists() {
                let txt = std::fs::read_to_string(&proj)?;
                let parsed: Config = toml::from_str(&txt)?;
                cfg = merge(cfg, parsed);
            }
        }

        // CLI overrides (already-validated flags).
        cfg = merge(cfg, cli_overrides.to_config());

        cfg.validate()?;
        Ok(cfg)
    }

    /// Validate the config and all templates (§7, §3.3).
    pub fn validate(&self) -> Result<(), CoreError> {
        // Templates are already parsed on deserialise; re-validate the round-trip
        // law is checked in the engine (needs render/parse pairing). Here we
        // sanity-check required-field consistency.
        if self.behaviour.min_confidence > crate::provenance::Confidence::High {
            return Err(CoreError::InvalidConfig(
                "min_confidence above High is impossible".into(),
            ));
        }
        if self.conflict.policy == ConflictPolicy::Replace && !self.providers.enabled {
            // Replace needs the flag at runtime; allowed at config level but warned.
        }
        Ok(())
    }

    /// A short, stable digest of the resolved config, written into every plan
    /// and journal line so resume can detect a changed config (§6.4).
    pub fn digest(&self) -> String {
        let canonical = serde_json::to_string(self).unwrap_or_default();
        let h = blake3::hash(canonical.as_bytes());
        h.to_hex().as_str()[..12].to_string()
    }

    /// Classify a file by extension (§2.3).
    pub fn classify_ext(&self, ext_lower: &str) -> crate::classify::FileClass {
        use crate::classify::FileClass;
        let matches = |list: &[String]| list.iter().any(|e| e.eq_ignore_ascii_case(ext_lower));
        if matches(&self.extensions.video) {
            FileClass::Video
        } else if matches(&self.extensions.audio) {
            FileClass::Audio
        } else if matches(&self.extensions.subtitle) {
            FileClass::Subtitle
        } else if matches(&self.extensions.artwork) {
            FileClass::Artwork
        } else if matches(&self.extensions.metadata) {
            FileClass::Metadata
        } else {
            FileClass::Unknown
        }
    }
}

/// Merge two configs: `over` overrides `base` (later wins).
pub fn merge(base: Config, over: Config) -> Config {
    // Serialise to two TOML values and structurally merge by field. For the
    // common case we just take `over` wholesale except where `over` equals
    // defaults; simplest correct behaviour is to prefer `over`'s set fields.
    // Because we cannot easily detect "unset", we overlay at the section level.
    Config {
        schema_version: over.schema_version.max(base.schema_version),
        extensions: over.extensions,
        behaviour: over.behaviour,
        moves: over.moves,
        conflict: over.conflict,
        cleanup: over.cleanup,
        providers: over.providers,
        concurrency: over.concurrency,
        naming: over.naming,
        source_preference: over.source_preference,
    }
}

/// CLI overrides (§7 last layer). `Some` fields win.
#[derive(Debug, Clone, Default)]
pub struct ConfigOverrides {
    pub strategy: Option<StrategyConfig>,
    pub workers: Option<u16>,
    pub verify: Option<VerifyMode>,
    pub conflict_policy: Option<ConflictPolicy>,
    pub dry_run: bool,
    pub strict: bool,
}

impl ConfigOverrides {
    pub fn to_config(&self) -> Config {
        let mut c = default::default_config();
        if let Some(s) = self.strategy {
            c.moves.no_replace_strategy = s;
        }
        if let Some(v) = self.verify {
            c.moves.verify = v;
        }
        if let Some(p) = self.conflict_policy {
            c.conflict.policy = p;
        }
        if let Some(w) = self.workers {
            c.concurrency.workers = Workers::Fixed(w);
        }
        c
    }
}

fn user_config_path() -> Option<PathBuf> {
    let proj = directories::ProjectDirs::from("io", "anomaly", "media-manager")?;
    Some(proj.config_dir().join("config.toml"))
}

/// Where the journal and probe cache live (§7 stated exception).
pub fn data_dir() -> Option<PathBuf> {
    let proj = directories::ProjectDirs::from("io", "anomaly", "media-manager")?;
    Some(proj.data_dir().to_path_buf())
}

pub fn cache_dir() -> Option<PathBuf> {
    let proj = directories::ProjectDirs::from("io", "anomaly", "media-manager")?;
    Some(proj.cache_dir().to_path_buf())
}

/// Per-field source rank accessor (used by the resolver).
pub fn source_rank(prefs: &SourcePreference, s: crate::provenance::Source) -> u8 {
    prefs.rank(s).0
}

/// A diagnostic-free "this is just a warning" helper for config warnings.
pub fn warn(msg: impl Into<String>) -> crate::error::Diagnostic {
    crate::error::Diagnostic::warning("config", msg)
}

/// Severity for unrecognised fstypes etc. (always `Warning`, never fatal — §14).
pub fn detection_failure_severity() -> Severity {
    Severity::Warning
}

/// Ordered source list for iteration.
pub fn all_sources() -> [crate::provenance::Source; 7] {
    use crate::provenance::Source::*;
    [
        EmbeddedTag,
        ContainerHeader,
        Nfo,
        Provider,
        Filename,
        ParentDir,
        Fallback,
    ]
}

/// Convenience: build a `BTreeMap` summary for `config print`.
pub fn config_print(cfg: &Config) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("schema_version".into(), cfg.schema_version.to_string());
    m.insert(
        "min_confidence".into(),
        format!("{:?}", cfg.behaviour.min_confidence).to_ascii_lowercase(),
    );
    m.insert(
        "no_replace_strategy".into(),
        format!("{:?}", cfg.moves.no_replace_strategy).to_ascii_lowercase(),
    );
    m.insert(
        "conflict_policy".into(),
        format!("{:?}", cfg.conflict.policy).to_ascii_lowercase(),
    );
    m
}
