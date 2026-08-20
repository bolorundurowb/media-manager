//! Build an in-place move plan under the library root.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::group::{GroupOutcome, MovieGroup, SkippedFolder, TvShowGroup};
use crate::parse::{
    episode_file_stem, extension_lower, movie_folder_name, parse_episode, season_folder_name,
    show_folder_name, version_label,
};
use crate::scan::{
    associated_subtitles, extra_files, is_subs_dir_name, list_subtitle_files, subtitle_suffix,
};

#[derive(Debug, Clone)]
pub struct MoveOp {
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Skip {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub dirs: Vec<PathBuf>,
    pub moves: Vec<MoveOp>,
    pub skips: Vec<Skip>,
    /// Source media folders that may be removed if empty after apply.
    pub source_folders: Vec<PathBuf>,
}

impl Plan {
    fn skip(&mut self, path: PathBuf, reason: impl Into<String>) {
        let reason = reason.into();
        tracing::warn!(path = %path.display(), %reason, "skip");
        self.skips.push(Skip { path, reason });
    }
}

pub fn build_plan(root: &Path, outcome: GroupOutcome, prior_skips: Vec<SkippedFolder>) -> Plan {
    let mut plan = Plan::default();
    for s in prior_skips {
        plan.skips.push(Skip {
            path: s.path,
            reason: s.reason,
        });
    }
    match outcome {
        GroupOutcome::Movies(groups) => {
            for g in groups {
                plan_movie_group(root, g, &mut plan);
            }
        }
        GroupOutcome::Tv(groups) => {
            for g in groups {
                plan_tv_group(root, g, &mut plan);
            }
        }
    }
    dedupe_dirs(&mut plan);
    validate_plan(root, &mut plan);
    plan
}

fn plan_movie_group(root: &Path, group: MovieGroup, plan: &mut Plan) {
    let folder_name = movie_folder_name(&group.title, group.year);
    let dest_dir = root.join(&folder_name);
    plan.dirs.push(dest_dir.clone());

    let mut claimed: HashSet<String> = HashSet::new();

    for folder in &group.folders {
        plan.source_folders.push(folder.folder.path.clone());
        let folder_version = version_label(&folder.parsed);
        // When a folder holds several videos, each must carry its own distinct
        // label from its filename; the folder-level label is only a fallback
        // for a lone video.
        let multiple = folder.folder.videos.len() > 1;
        let mut videos_ok = 0usize;
        for video in &folder.folder.videos {
            let file_parsed = video.file_name().and_then(|n| n.to_str()).and_then(|n| {
                crate::parse::parse_media_name(n, crate::parse::LibraryKind::Movies).ok()
            });
            let file_version = file_parsed.as_ref().and_then(version_label);
            let version = file_version.clone().or_else(|| {
                if multiple {
                    None
                } else {
                    folder_version.clone()
                }
            });
            let Some(version) = version else {
                plan.skip(
                    video.clone(),
                    "no reliable version label; refusing to invent one",
                );
                continue;
            };
            let Some(ext) = extension_lower(video) else {
                plan.skip(video.clone(), "missing file extension");
                continue;
            };
            let dest_name = format!("{folder_name} - {version}.{ext}");
            let dest = dest_dir.join(&dest_name);
            if video.as_path() == dest.as_path() {
                tracing::debug!(path = %video.display(), "already at destination");
                videos_ok += 1;
                continue;
            }
            if !claim_dest(plan, &mut claimed, video, &dest) {
                continue;
            }
            plan_video_and_subs(plan, video, &dest, &mut claimed);
            videos_ok += 1;
        }
        let sources = plan_move_sources(plan);
        log_unassociated_subs(&folder.folder.path, &folder.folder.videos, &sources, plan);
        if videos_ok == folder.folder.videos.len() && videos_ok > 0 {
            plan_extras(
                &folder.folder.path,
                &folder.folder.videos,
                &dest_dir,
                plan,
                &mut claimed,
            );
        }
    }
}

fn plan_tv_group(root: &Path, group: TvShowGroup, plan: &mut Plan) {
    let show_dir_name = show_folder_name(&group.title, group.year);
    let show_dir = root.join(&show_dir_name);
    plan.dirs.push(show_dir.clone());

    let mut claimed: HashSet<String> = HashSet::new();

    for (season, folders) in &group.seasons {
        let season_dir = show_dir.join(season_folder_name(*season));
        plan.dirs.push(season_dir.clone());

        for folder in folders {
            plan.source_folders.push(folder.folder.path.clone());
            let mut videos_ok = 0usize;
            for video in &folder.folder.videos {
                let Some(name) = video.file_name().and_then(|n| n.to_str()) else {
                    plan.skip(video.clone(), "non-utf8 filename");
                    continue;
                };
                let Some(ep) = parse_episode(name) else {
                    plan.skip(video.clone(), "could not read SxxExx / NxNN episode number");
                    continue;
                };
                if ep.season != *season {
                    plan.skip(
                        video.clone(),
                        format!(
                            "file season {} does not match folder season {season}",
                            ep.season
                        ),
                    );
                    continue;
                }
                let Some(ext) = extension_lower(video) else {
                    plan.skip(video.clone(), "missing file extension");
                    continue;
                };
                let dest_name = format!("{}.{ext}", episode_file_stem(&group.title, &ep));
                let dest = season_dir.join(dest_name);
                if video.as_path() == dest.as_path() {
                    tracing::debug!(path = %video.display(), "already at destination");
                    videos_ok += 1;
                    continue;
                }
                if !claim_dest(plan, &mut claimed, video, &dest) {
                    continue;
                }
                plan_video_and_subs(plan, video, &dest, &mut claimed);
                videos_ok += 1;
            }
            let sources = plan_move_sources(plan);
            log_unassociated_subs(&folder.folder.path, &folder.folder.videos, &sources, plan);
            if videos_ok == folder.folder.videos.len() && videos_ok > 0 {
                plan_extras(
                    &folder.folder.path,
                    &folder.folder.videos,
                    &season_dir,
                    plan,
                    &mut claimed,
                );
            }
        }
    }
}

fn claim_dest(plan: &mut Plan, claimed: &mut HashSet<String>, from: &Path, dest: &Path) -> bool {
    if from == dest {
        tracing::debug!(path = %from.display(), "already at destination");
        return false;
    }
    if dest.exists() {
        plan.skip(
            from.to_path_buf(),
            format!("destination already exists: {}", dest.display()),
        );
        return false;
    }
    // Windows and macOS filesystems are case-insensitive by default, and
    // this project targets Windows first, so two planned destinations that
    // differ only by case are treated as a collision everywhere, not just
    // on the platform running the plan.
    if !claimed.insert(dest_key(dest)) {
        plan.skip(
            from.to_path_buf(),
            format!("duplicate destination in plan: {}", dest.display()),
        );
        return false;
    }
    true
}

/// Case-folded comparison key for a destination path, used to catch
/// destinations that differ only by case (a collision on case-insensitive
/// filesystems).
pub(crate) fn dest_key(path: &Path) -> String {
    path.to_string_lossy().to_ascii_lowercase()
}

/// Conservative path-length ceiling. Windows historically caps full paths at
/// 260 characters without opting into long-path support; other platforms
/// are far more permissive, but a generous fixed ceiling still catches
/// pathological cases without needing per-OS detection at runtime.
#[cfg(windows)]
const MAX_PATH_LEN: usize = 260;
#[cfg(not(windows))]
const MAX_PATH_LEN: usize = 4096;

/// Most filesystems (NTFS, ext4, APFS, ...) cap a single path component at
/// 255 bytes/UTF-16 units.
const MAX_COMPONENT_LEN: usize = 255;

pub(crate) fn path_too_long(path: &Path) -> bool {
    if path.as_os_str().len() >= MAX_PATH_LEN {
        return true;
    }
    path.components()
        .any(|c| c.as_os_str().len() > MAX_COMPONENT_LEN)
}

fn plan_video_and_subs(
    plan: &mut Plan,
    video: &Path,
    dest: &Path,
    claimed: &mut HashSet<String>,
) {
    plan.moves.push(MoveOp {
        from: video.to_path_buf(),
        to: dest.to_path_buf(),
    });

    let Some(video_stem) = video.file_stem().and_then(|s| s.to_str()) else {
        return;
    };
    let Some(dest_stem) = dest.file_stem().and_then(|s| s.to_str()) else {
        return;
    };
    let Some(dest_parent) = dest.parent() else {
        return;
    };

    for sub in associated_subtitles(video) {
        let Some(sub_name) = sub.path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(suffix) = subtitle_suffix(sub_name, video_stem) else {
            continue;
        };
        let Some(ext) = extension_lower(&sub.path) else {
            continue;
        };
        let dest_dir = if sub.nested {
            let subs_dir = dest_parent.join("subs");
            if !plan.dirs.iter().any(|d| d == &subs_dir) {
                plan.dirs.push(subs_dir.clone());
            }
            subs_dir
        } else {
            dest_parent.to_path_buf()
        };
        let sub_dest = dest_dir.join(format!("{dest_stem}{suffix}.{ext}"));
        if !claim_dest(plan, claimed, &sub.path, &sub_dest) {
            continue;
        }
        plan.moves.push(MoveOp {
            from: sub.path,
            to: sub_dest,
        });
    }
}

fn plan_move_sources(plan: &Plan) -> HashSet<PathBuf> {
    plan.moves.iter().map(|m| m.from.clone()).collect()
}

fn log_unassociated_subs(
    folder: &Path,
    videos: &[PathBuf],
    planned_from: &HashSet<PathBuf>,
    plan: &mut Plan,
) {
    let mut dirs = vec![folder.to_path_buf()];
    for video in videos {
        if let Some(parent) = video.parent() {
            if !dirs.iter().any(|d| d == parent) {
                dirs.push(parent.to_path_buf());
            }
        }
    }
    let mut seen = HashSet::new();
    for dir in dirs {
        for sub in list_subtitle_files(&dir) {
            if !seen.insert(sub.clone()) {
                continue;
            }
            if planned_from.contains(&sub) {
                continue;
            }
            if plan.skips.iter().any(|s| s.path == sub) {
                continue;
            }
            plan.skip(sub, "unassociated subtitle; leaving in place");
        }
    }
}

fn plan_extras(
    folder: &Path,
    videos: &[PathBuf],
    dest_dir: &Path,
    plan: &mut Plan,
    claimed: &mut HashSet<String>,
) {
    let mut dirs = vec![folder.to_path_buf()];
    for video in videos {
        if let Some(parent) = video.parent() {
            if !dirs.iter().any(|d| d == parent) {
                dirs.push(parent.to_path_buf());
            }
        }
    }
    let planned_from = plan_move_sources(plan);
    for dir in dirs {
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if is_subs_dir_name(name) {
            continue;
        }
        for extra in extra_files(&dir) {
            if planned_from.contains(&extra) {
                continue;
            }
            let Some(file_name) = extra.file_name() else {
                continue;
            };
            let dest = dest_dir.join(file_name);
            if extra == dest {
                continue;
            }
            if !claim_dest(plan, claimed, &extra, &dest) {
                continue;
            }
            plan.moves.push(MoveOp {
                from: extra,
                to: dest,
            });
        }
    }
}

fn dedupe_dirs(plan: &mut Plan) {
    let mut seen = HashSet::new();
    plan.dirs.retain(|d| seen.insert(d.clone()));
    plan.source_folders.sort();
    plan.source_folders.dedup();
}

fn validate_plan(root: &Path, plan: &mut Plan) {
    let mut drop_moves = Vec::new();
    for (i, mv) in plan.moves.iter().enumerate() {
        if !mv.from.exists() {
            drop_moves.push((i, format!("source missing: {}", mv.from.display())));
            continue;
        }
        if !is_under(root, &mv.to) {
            drop_moves.push((
                i,
                format!("destination escapes library root: {}", mv.to.display()),
            ));
            continue;
        }
        if path_too_long(&mv.to) {
            drop_moves.push((i, format!("destination path too long: {}", mv.to.display())));
            continue;
        }
        if would_move_into_self(&mv.from, &mv.to) {
            drop_moves.push((
                i,
                format!(
                    "refusing to move a path into itself: {} -> {}",
                    mv.from.display(),
                    mv.to.display()
                ),
            ));
        }
    }
    for (idx, reason) in drop_moves.into_iter().rev() {
        let mv = plan.moves.remove(idx);
        plan.skip(mv.from, reason);
    }
}

/// True when `path` is `root` or a descendant of `root`.
fn is_under(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
}

/// True when the destination is the source or is nested under it (for
/// example `file.mkv` → `file.mkv/child`), which would move a path into
/// itself. A rename *inside* the source's parent directory is allowed.
fn would_move_into_self(from: &Path, to: &Path) -> bool {
    if from == to {
        return true;
    }
    is_under(from, to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn dest_key_treats_case_as_a_collision() {
        assert_eq!(
            dest_key(Path::new(r"C:\lib\Movie\Cover.jpg")),
            dest_key(Path::new(r"C:\lib\Movie\cover.jpg"))
        );
        assert_ne!(
            dest_key(Path::new(r"C:\lib\Movie\Cover.jpg")),
            dest_key(Path::new(r"C:\lib\Movie\poster.jpg"))
        );
    }

    #[test]
    fn path_too_long_rejects_oversized_component() {
        let component = "x".repeat(MAX_COMPONENT_LEN + 1);
        let path = PathBuf::from("root").join(component);
        assert!(path_too_long(&path));
        assert!(!path_too_long(Path::new("root/short.mkv")));
    }

    #[test]
    fn path_too_long_rejects_oversized_full_path() {
        let path = PathBuf::from("x".repeat(MAX_PATH_LEN));
        assert!(path_too_long(&path));
    }

    #[test]
    fn move_into_self_detects_nested_dest() {
        let from = Path::new("/lib/video.mkv");
        let to = Path::new("/lib/video.mkv/nested");
        assert!(would_move_into_self(from, to));
        assert!(!would_move_into_self(
            Path::new("/lib/a/video.mkv"),
            Path::new("/lib/b/video.mkv")
        ));
    }
}
