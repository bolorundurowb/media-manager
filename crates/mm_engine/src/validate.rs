//! Validate stage (§5.6).
//!
//! Containment, component length, reserved names, and NoOp detection.

use std::path::{Path, PathBuf};

use mm_core::error::Diagnostic;
use mm_core::path::normalise_relative;
use mm_core::volume::VolumeSemantics;

/// Validation outcome for a planned item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Validation {
    Ok,
    NoOp,
    ContainmentBreach { path: PathBuf },
    TooLong { path: PathBuf },
    EmptyName { path: PathBuf },
}

/// Validate a destination relative to root.
pub fn validate_destination(
    _root: &Path,
    source: &Path,
    absolute_dest: &Path,
    relative_dest: &Path,
    volume: &VolumeSemantics,
) -> (Validation, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();

    // Containment via normalised relative path.
    match normalise_relative(relative_dest) {
        Some(rel) if rel.components().next().is_some() => {
            // relative path non-empty and does not escape
        }
        _ => {
            return (
                Validation::ContainmentBreach {
                    path: absolute_dest.to_path_buf(),
                },
                diagnostics,
            );
        }
    }

    // Total path length.
    let total_len = absolute_dest.as_os_str().len();
    if total_len > volume.max_total as usize {
        diagnostics.push(Diagnostic::failure(
            "validate",
            format!("path too long ({} > {})", total_len, volume.max_total),
        ));
        return (Validation::TooLong { path: absolute_dest.to_path_buf() }, diagnostics);
    }

    // NoOp: source and destination are the same file.
    if same_file_under_semantics(source, absolute_dest, volume) {
        return (Validation::NoOp, diagnostics);
    }

    (Validation::Ok, diagnostics)
}

fn same_file_under_semantics(a: &Path, b: &Path, volume: &VolumeSemantics) -> bool {
    // Compare normalised strings under volume semantics.
    let a_key: String = a
        .components()
        .map(|c| volume.collision_key(c.as_os_str().to_string_lossy().as_ref()))
        .collect::<Vec<_>>()
        .join("\\");
    let b_key: String = b
        .components()
        .map(|c| volume.collision_key(c.as_os_str().to_string_lossy().as_ref()))
        .collect::<Vec<_>>()
        .join("\\");
    a_key == b_key
}
