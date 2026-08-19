//! Scan → classify → group → plan → execute engine (§5, §6).
//!
//! Phase 2 implements the planning pipeline for movies: scan, classify, parse,
//! group, resolve, regroup, associate, route, validate, reconcile, and
//! rendering (dry-run / GUI preview / --json).

pub mod classify;
pub mod group;
pub mod planner;
pub mod reconcile;
pub mod render;
pub mod resolve;
pub mod route;
pub mod scan;
pub mod validate;

pub use planner::{PlanOptions, Planner};
pub use render::{render_json, render_text};
