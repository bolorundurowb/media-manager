//! Core domain model for media-manager (§2, §14).
//!
//! Provenance-bearing fields, identity keys, classification, the path layer,
//! the `FileSystem` abstraction, the error taxonomy, and layered config.

pub mod classify;
pub mod config;
pub mod error;
pub mod fs;
pub mod identity;
pub mod path;
pub mod plan;
pub mod provenance;
pub mod template;
pub mod volume;

pub use classify::{FileClass, MediaKind};
pub use config::Config;
pub use error::{Diagnostic, FatalReason, Outcome, RunMode, RunReport, Severity};
pub use fs::{CancelToken, DirEntry, FileId, FileMeta, FileSystem, Hash, ReadDirIter};
pub use identity::{
    AlbumId, Edition, EpisodeId, MovieId, Norm, NormOptions, ShowId, SourcePreference, TrackId,
};
pub use plan::{Action, DirRemoval, DirRename, Plan, PlanItem, PlanStats, Readiness};
pub use provenance::{Confidence, Field, MinSource, Source};
pub use template::Template;
pub use volume::{ComponentLimit, NoReplaceStrategy, VolumeSemantics};
