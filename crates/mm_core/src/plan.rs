//! Plan and readiness types (§5.7, §5.2).
//!
//! The `Plan` is a serialisable value — one artifact serves dry-run output, GUI
//! preview, `--json` for automation, an on-disk plan file, and resume. Dry-run
//! and apply provably share planning logic because apply consumes the same
//! struct.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::classify::MediaKind;
use crate::error::Diagnostic;
use crate::volume::VolumeSemantics;

/// Stable identifier for a plan item. The GUI deselects items by id (§9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DirRenameId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DirRemovalId(pub u64);

impl ItemId {
    pub fn new(n: u64) -> Self {
        ItemId(n)
    }
}

/// A named field, used in `NeedsReview` (§5.2).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldName {
    Title,
    Year,
    Edition,
    Season,
    Episodes,
    EpisodeTitle,
    AlbumArtist,
    Album,
    Track,
    TrackTitle,
    Disc,
    Resolution,
}

impl FieldName {
    pub fn as_str(&self) -> &'static str {
        match self {
            FieldName::Title => "title",
            FieldName::Year => "year",
            FieldName::Edition => "edition",
            FieldName::Season => "season",
            FieldName::Episodes => "episodes",
            FieldName::EpisodeTitle => "episode_title",
            FieldName::AlbumArtist => "album_artist",
            FieldName::Album => "album",
            FieldName::Track => "track",
            FieldName::TrackTitle => "track_title",
            FieldName::Disc => "disc",
            FieldName::Resolution => "resolution",
        }
    }
}

/// Readiness of a resolved item (§5.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum Readiness {
    Ready,
    NeedsReview {
        missing: Vec<FieldName>,
        reasons: Vec<String>,
    },
    Ambiguous {
        interpretations: Vec<String>,
    },
}

/// What to do with a planned item (§5.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", content = "data")]
pub enum Action {
    NoOp,
    Move {
        from: PathBuf,
        to: PathBuf,
    },
    Skip {
        reason: SkipReason,
    },
    Conflict {
        from: PathBuf,
        to: PathBuf,
        existing: ExistingInfo,
    },
    Duplicate {
        from: PathBuf,
        identical_to: PathBuf,
    },
    NeedsReview {
        path: PathBuf,
        missing: Vec<FieldName>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    Symlink,
    Ignored,
    Junk,
    Unknown,
    Protected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExistingInfo {
    pub path: PathBuf,
    pub len: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blake3: Option<String>,
}

/// A directory rename (case/normalisation-only fix, §5.6/§5.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirRename {
    pub id: DirRenameId,
    pub from: PathBuf,
    pub to: PathBuf,
}

/// A directory removal candidate (deepest-first, §6.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirRemoval {
    pub id: DirRemovalId,
    pub path: PathBuf,
}

/// One planned item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanItem {
    pub id: ItemId,
    pub source: PathBuf,
    pub relative: PathBuf,
    pub class: crate::classify::FileClass,
    pub readiness: Readiness,
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<PathBuf>,
}

/// Summary counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStats {
    pub total: u64,
    pub ready: u64,
    pub noop: u64,
    pub needs_review: u64,
    pub ambiguous: u64,
    pub conflicts: u64,
    pub duplicates: u64,
    pub skipped: u64,
    pub unclassified: u64,
}

/// The serialisable plan (§5.7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub version: u32,
    pub run_id: Uuid,
    pub root: PathBuf,
    pub kind: MediaKind,
    pub config_digest: String,
    pub volume: VolumeSemantics,
    pub items: Vec<PlanItem>,
    pub dir_creates: BTreeSet<PathBuf>,
    pub dir_renames: Vec<DirRename>,
    pub dir_removals: Vec<DirRemoval>,
    pub diagnostics: Vec<Diagnostic>,
    pub stats: PlanStats,
}

impl Plan {
    pub fn new(
        run_id: Uuid,
        root: PathBuf,
        kind: MediaKind,
        config_digest: String,
        volume: VolumeSemantics,
    ) -> Self {
        Plan {
            version: 1,
            run_id,
            root,
            kind,
            config_digest,
            volume,
            items: Vec::new(),
            dir_creates: BTreeSet::new(),
            dir_renames: Vec::new(),
            dir_removals: Vec::new(),
            diagnostics: Vec::new(),
            stats: PlanStats::default(),
        }
    }
}
