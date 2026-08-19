//! Error taxonomy and reporting (§14).
//!
//! Errors are classified by **blast radius**, not by cause. What the caller
//! needs to know is whether to keep going, not which syscall failed. Only
//! [`Severity::Fatal`] stops a run; its membership is deliberately tiny.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::classify::MediaKind;
use crate::plan::ItemId;

/// Severity by blast radius.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Recorded, not surfaced by default (NoOp, cache hit).
    Info,
    /// Per-item, expected: Skip, Unknown file, orphan subtitle.
    Notice,
    /// Per-item, actionable: NeedsReview, Ambiguous, dir not removable,
    /// resolution undetectable.
    Warning,
    /// Per-item, unexpected: the item did not complete.
    Failure,
    /// Run-scoped: continuing would risk the library.
    Fatal,
}

/// Why a run aborted. The full `Fatal` set is small by design (§14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", content = "data")]
pub enum FatalReason {
    /// Root missing, unreadable, or not a directory.
    RootUnusable { root: PathBuf, why: String },
    /// The journal could not be created or fsynced.
    JournalUnwritable { path: PathBuf, why: String },
    /// Resume found both source and destination missing — data loss.
    ResumeLoss { from: PathBuf, to: PathBuf },
    /// A destination canonicalises outside root (§5.6 containment).
    ContainmentBreach { path: PathBuf },
}

/// A per-item outcome. The derived `Ord` is load-bearing: it is the
/// [`BTreeMap`] key in [`RunReport`] and the sort order in the GUI table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    NoOp,
    Moved,
    Skipped,
    Unclassified,
    NeedsReview,
    Ambiguous,
    Conflicted,
    Duplicated,
    Failed,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::NoOp => "noop",
            Outcome::Moved => "moved",
            Outcome::Skipped => "skipped",
            Outcome::Unclassified => "unclassified",
            Outcome::NeedsReview => "review",
            Outcome::Ambiguous => "ambiguous",
            Outcome::Conflicted => "conflict",
            Outcome::Duplicated => "duplicate",
            Outcome::Failed => "failed",
        }
    }

    /// The severity for this outcome, used by `--strict` promotion.
    pub fn severity(self) -> Severity {
        match self {
            Outcome::NoOp => Severity::Info,
            Outcome::Moved => Severity::Info,
            Outcome::Skipped => Severity::Notice,
            Outcome::Unclassified => Severity::Notice,
            Outcome::NeedsReview | Outcome::Ambiguous => Severity::Warning,
            Outcome::Conflicted | Outcome::Duplicated => Severity::Warning,
            Outcome::Failed => Severity::Failure,
        }
    }
}

/// What mode a run executed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    Plan,
    DryRun,
    Apply,
    Verify,
    Resume,
}

/// A diagnostic line, attached to an item or run-scoped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    /// `None` for run-scoped diagnostics.
    pub item: Option<ItemId>,
    /// Which pipeline stage produced this.
    pub stage: String,
    pub message: String,
    /// The underlying io kind, when relevant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_kind: Option<String>,
}

impl Diagnostic {
    pub fn info(stage: impl Into<String>, message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Info,
            item: None,
            stage: stage.into(),
            message: message.into(),
            io_kind: None,
        }
    }
    pub fn warning(stage: impl Into<String>, message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Warning,
            item: None,
            stage: stage.into(),
            message: message.into(),
            io_kind: None,
        }
    }
    pub fn failure(stage: impl Into<String>, message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Failure,
            item: None,
            stage: stage.into(),
            message: message.into(),
            io_kind: None,
        }
    }
}

/// The single value feeding CLI exit codes (§8), `--json` and the GUI stats
/// panel — so CLI and GUI cannot disagree about what happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub run_id: uuid::Uuid,
    pub mode: RunMode,
    pub kind: MediaKind,
    pub root: PathBuf,
    pub counts: BTreeMap<Outcome, u64>,
    /// Planned-but-not-applied work; the basis for exit 10.
    pub pending: BTreeMap<Outcome, u64>,
    pub diagnostics: Vec<Diagnostic>,
    pub dirs_removed: u64,
    pub dirs_not_removable: Vec<(PathBuf, String)>,
    pub reservations_reclaimed: u64,
    pub fatal: Option<FatalReason>,
    pub cancelled: bool,
    pub duration: Duration,
}

impl RunReport {
    pub fn new(run_id: uuid::Uuid, mode: RunMode, kind: MediaKind, root: PathBuf) -> Self {
        RunReport {
            run_id,
            mode,
            kind,
            root,
            counts: BTreeMap::new(),
            pending: BTreeMap::new(),
            diagnostics: Vec::new(),
            dirs_removed: 0,
            dirs_not_removable: Vec::new(),
            reservations_reclaimed: 0,
            fatal: None,
            cancelled: false,
            duration: Duration::ZERO,
        }
    }

    pub fn count(&mut self, outcome: Outcome) {
        *self.counts.entry(outcome).or_insert(0) += 1;
    }

    pub fn pending_count(&mut self, outcome: Outcome) {
        *self.pending.entry(outcome).or_insert(0) += 1;
    }

    pub fn total(&self, outcome: Outcome) -> u64 {
        self.counts.get(&outcome).copied().unwrap_or(0)
    }

    /// Compute the exit code per §8's precedence rule (highest applicable wins).
    pub fn exit_code(&self, strict: bool) -> u8 {
        if self.cancelled {
            return 130;
        }
        if self.fatal.is_some() {
            return 50;
        }
        if strict {
            if self.total(Outcome::Failed) > 0 {
                return 40;
            }
            if self.total(Outcome::Conflicted) > 0 {
                return 30;
            }
            if self.total(Outcome::Duplicated) > 0 {
                return 25;
            }
            if self.total(Outcome::NeedsReview) + self.total(Outcome::Ambiguous) > 0 {
                return 20;
            }
            if self.total(Outcome::Skipped) > 0 {
                return 20;
            }
        } else {
            if self.total(Outcome::Failed) > 0 {
                return 40;
            }
            if self.total(Outcome::Conflicted) > 0 {
                return 30;
            }
            if self.total(Outcome::Duplicated) > 0 {
                return 25;
            }
            if self.total(Outcome::NeedsReview) + self.total(Outcome::Ambiguous) > 0 {
                return 20;
            }
        }
        // Pending work (plan/verify): exit 10
        let total_pending: u64 = self.pending.values().sum();
        if total_pending > 0 {
            return 10;
        }
        0
    }
}

/// Errors internal to the core domain model.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid template: {0}")]
    InvalidTemplate(String),
    #[error("path sanitises to empty: {0}")]
    EmptyName(String),
    #[error("destination occupied: {0}")]
    DestinationOccupied(PathBuf),
    #[error("source missing: {0}")]
    SourceMissing(PathBuf),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("toml error: {0}")]
    Toml(#[from] toml::de::Error),
}
