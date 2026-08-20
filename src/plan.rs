//! Build an in-place move plan under the library root.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::group::{GroupOutcome, MovieGroup, SkippedFolder, TvShowGroup};
use crate::parse::{
    episode_file_stem, extension_lower, movie_folder_name, parse_episode, season_folder_name,
    show_folder_name, version_label,
};
use crate::scan::{adjacent_subtitles, subtitle_suffix};

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

    let mut claimed: HashSet<PathBuf> = HashSet::new();

    for folder in &group.folders {
        plan.source_folders.push(folder.folder.path.clone());
        let folder_version = version_label(&folder.parsed);
        for video in &folder.folder.videos {
            let file_parsed = video.file_name().and_then(|n| n.to_str()).and_then(|n| {
                crate::parse::parse_media_name(n, crate::parse::LibraryKind::Movies).ok()
            });
            let version = file_parsed
                .as_ref()
                .and_then(version_label)
                .or_else(|| folder_version.clone());
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
            if !claim_dest(plan, &mut claimed, video, &dest) {
                continue;
            }
            plan_video_and_subs(plan, video, &dest);
        }
    }
}

fn plan_tv_group(root: &Path, group: TvShowGroup, plan: &mut Plan) {
    let show_dir_name = show_folder_name(&group.title, group.year);
    let show_dir = root.join(&show_dir_name);
    plan.dirs.push(show_dir.clone());

    let mut claimed: HashSet<PathBuf> = HashSet::new();

    for (season, folders) in &group.seasons {
        let season_dir = show_dir.join(season_folder_name(*season));
        plan.dirs.push(season_dir.clone());

        for folder in folders {
            plan.source_folders.push(folder.folder.path.clone());
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
                if !claim_dest(plan, &mut claimed, video, &dest) {
                    continue;
                }
                plan_video_and_subs(plan, video, &dest);
            }
        }
    }
}

fn claim_dest(plan: &mut Plan, claimed: &mut HashSet<PathBuf>, from: &Path, dest: &Path) -> bool {
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
    if !claimed.insert(dest.to_path_buf()) {
        plan.skip(
            from.to_path_buf(),
            format!("duplicate destination in plan: {}", dest.display()),
        );
        return false;
    }
    true
}

fn plan_video_and_subs(plan: &mut Plan, video: &Path, dest: &Path) {
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

    for sub in adjacent_subtitles(video) {
        let Some(sub_name) = sub.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(suffix) = subtitle_suffix(sub_name, video_stem) else {
            continue;
        };
        let Some(ext) = extension_lower(&sub) else {
            continue;
        };
        let sub_dest = dest_parent.join(format!("{dest_stem}{suffix}.{ext}"));
        if sub == sub_dest {
            continue;
        }
        if sub_dest.exists() {
            plan.skip(
                sub,
                format!(
                    "subtitle destination already exists: {}",
                    sub_dest.display()
                ),
            );
            continue;
        }
        plan.moves.push(MoveOp {
            from: sub,
            to: sub_dest,
        });
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
        if let Some(parent) = mv.from.parent() {
            if is_under(&mv.to, parent) && mv.to != *parent {
                // dest file inside its source folder is OK (rename in place).
                // dest directory must not be a descendant in a way that moves a dir into itself;
                // we only move files.
            }
        }
        if is_under(&mv.from, &mv.to) {
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

fn is_under(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
}
