//! Scan → classify → group → plan → execute engine (§5, §6).
//!
//! Planning pipeline for movies: scan, classify, parse, group, probe, resolve,
//! regroup, associate, route, validate, reconcile, and rendering.
//! Phase 3 adds journaled execution, `verify`, and `gc`. Phase 4 adds
//! container probing (stage 5) with a `(file_id, size, mtime)` cache.

pub mod classify;
pub mod exec;
pub mod group;
pub mod journal;
pub mod planner;
pub mod probe_stage;
pub mod reconcile;
pub mod render;
pub mod resolve;
pub mod route;
pub mod scan;
pub mod validate;

pub use exec::{ExecOptions, GcOptions, execute, gc, report_from_plan};
pub use journal::{Journal, JournalEntry, JournalOp, JournalPhase};
pub use planner::{PlanOptions, Planner};
pub use render::{render_json, render_text};
