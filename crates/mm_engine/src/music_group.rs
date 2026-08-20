//! Music group stage (Phase 6, §5.3, §2.2).
//!
//! Two-pass grouping mirroring `crate::group::group_movies`: pass 1 keys on
//! the mandatory discriminators (`album_artist` + `album`, normalised),
//! collecting candidate years; pass 2 resolves one canonical year per group
//! and writes it back, keying the final `AlbumId` on the complete identity.
//!
//! Unlike movies, this function is called *twice* in the music pipeline
//! (`music_plan`): once pre-probe on filename-only candidates (mostly a
//! formality — music filenames rarely carry album/artist at all, so this
//! pass groups few if any tracks; its main job is deciding which items are
//! worth the probe, and for audio that gate is simply "is an audio file", see
//! `music_plan` module docs) and once post-probe on tag-resolved candidates,
//! which is where the real grouping happens.

use std::collections::HashMap;

use mm_core::identity::{AlbumId, Norm};

use crate::music_resolve::ResolvedTrack;

/// A group of tracks sharing an album identity.
#[derive(Debug, Clone)]
pub struct AlbumGroup {
    pub id: AlbumId,
    pub items: Vec<usize>, // indices into the planner's item list
}

/// Group resolved tracks by album identity. Items missing either mandatory
/// discriminator (`album_artist` or `album`) are excluded from every group —
/// they cannot form a meaningful pass-1 key, and `ResolvedTrack::readiness`
/// already routes them to `NeedsReview` independently of grouping.
pub fn group_albums(items: &[(usize, &ResolvedTrack)]) -> Vec<AlbumGroup> {
    // Pass 1: provisional groups by album_artist + album.
    let mut by_key: HashMap<(Norm, Norm), Vec<usize>> = HashMap::new();
    for (idx, t) in items {
        let (Some(artist), Some(album)) = (t.album_artist.as_value(), t.album.as_value()) else {
            continue;
        };
        let artist_norm = Norm::from_display(artist);
        let album_norm = Norm::from_display(album);
        by_key.entry((artist_norm, album_norm)).or_default().push(*idx);
    }

    // Pass 2: resolve canonical year per group and build the complete AlbumId.
    let mut groups = Vec::new();
    for ((artist_norm, album_norm), indices) in by_key {
        let years: Vec<u16> = indices
            .iter()
            .filter_map(|i| {
                items
                    .iter()
                    .find(|(idx, _)| idx == i)
                    .and_then(|(_, t)| t.year.as_value().copied())
            })
            .collect();
        let canonical_year = pick_canonical_year(&years);

        let mut id = AlbumId::new(artist_norm, album_norm);
        id.year = canonical_year;

        groups.push(AlbumGroup { id, items: indices });
    }

    groups
}

fn pick_canonical_year(years: &[u16]) -> Option<u16> {
    if years.is_empty() {
        return None;
    }
    // Majority, same as `crate::group::group_movies`'s pass 2.
    let mut counts: HashMap<u16, usize> = HashMap::new();
    for y in years {
        *counts.entry(*y).or_default() += 1;
    }
    counts.into_iter().max_by_key(|(_, c)| *c).map(|(y, _)| y)
}

/// `true` if this album group has a mix of tracks with a known disc number
/// and tracks without one (§5.5's "multi-disc collision" rule): partial disc
/// information is `NeedsReview` rather than silently collapsing into one
/// directory where `01 - X` and `01 - Y` from different discs collide.
pub fn has_partial_disc_info(items: &[usize], tracks: &[(usize, &ResolvedTrack)]) -> bool {
    let mut any_known = false;
    let mut any_unknown = false;
    for idx in items {
        let Some((_, t)) = tracks.iter().find(|(i, _)| i == idx) else {
            continue;
        };
        if t.disc.is_known() {
            any_known = true;
        } else {
            any_unknown = true;
        }
    }
    any_known && any_unknown
}

/// `true` if this album group has more than one distinct known disc number —
/// only then does `disc_dir` apply (§5.5: "only when the album spans
/// multiple discs").
pub fn is_multi_disc(items: &[usize], tracks: &[(usize, &ResolvedTrack)]) -> bool {
    let mut discs: Vec<u16> = items
        .iter()
        .filter_map(|idx| {
            tracks
                .iter()
                .find(|(i, _)| i == idx)
                .and_then(|(_, t)| t.disc.as_value().copied())
        })
        .collect();
    discs.sort_unstable();
    discs.dedup();
    discs.len() > 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_core::provenance::{Confidence, Field, Source};

    fn track(album_artist: Option<&str>, album: Option<&str>, year: Option<u16>) -> ResolvedTrack {
        let mut t = ResolvedTrack::default();
        if let Some(a) = album_artist {
            t.album_artist = Field::known(a.to_string(), Source::EmbeddedTag, Confidence::High);
        }
        if let Some(a) = album {
            t.album = Field::known(a.to_string(), Source::EmbeddedTag, Confidence::High);
        }
        if let Some(y) = year {
            t.year = Field::known(y, Source::EmbeddedTag, Confidence::High);
        }
        t
    }

    #[test]
    fn groups_by_album_artist_and_album() {
        let a = track(Some("Nina Simone"), Some("Wild Is the Wind"), Some(1966));
        let b = track(Some("Nina Simone"), Some("Wild Is the Wind"), Some(1966));
        let c = track(Some("Miles Davis"), Some("Kind of Blue"), Some(1959));
        let items = vec![(0, &a), (1, &b), (2, &c)];
        let groups = group_albums(&items);
        assert_eq!(groups.len(), 2);
        let nina = groups
            .iter()
            .find(|g| g.id.album_artist.key.contains("nina"))
            .unwrap();
        assert_eq!(nina.items.len(), 2);
    }

    #[test]
    fn items_missing_either_discriminator_are_excluded() {
        let a = track(Some("Artist"), None, None);
        let b = track(None, Some("Album"), None);
        let items = vec![(0, &a), (1, &b)];
        let groups = group_albums(&items);
        assert!(groups.is_empty());
    }

    #[test]
    fn canonical_year_is_majority() {
        let a = track(Some("X"), Some("Y"), Some(2000));
        let b = track(Some("X"), Some("Y"), Some(2000));
        let c = track(Some("X"), Some("Y"), Some(1999));
        let items = vec![(0, &a), (1, &b), (2, &c)];
        let groups = group_albums(&items);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].id.year, Some(2000));
    }

    #[test]
    fn partial_disc_info_detected() {
        let mut a = track(Some("X"), Some("Y"), None);
        a.disc = Field::known(1u16, Source::EmbeddedTag, Confidence::High);
        let b = track(Some("X"), Some("Y"), None);
        let tracks = vec![(0, &a), (1, &b)];
        assert!(has_partial_disc_info(&[0, 1], &tracks));
        assert!(!has_partial_disc_info(&[0], &tracks));
    }

    #[test]
    fn multi_disc_detection() {
        let mut a = track(Some("X"), Some("Y"), None);
        a.disc = Field::known(1u16, Source::EmbeddedTag, Confidence::High);
        let mut b = track(Some("X"), Some("Y"), None);
        b.disc = Field::known(2u16, Source::EmbeddedTag, Confidence::High);
        let tracks = vec![(0, &a), (1, &b)];
        assert!(is_multi_disc(&[0, 1], &tracks));
        assert!(!is_multi_disc(&[0], &tracks));
    }
}
