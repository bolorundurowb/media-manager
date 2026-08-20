//! TV group stage (§5.3, §2.2, Phase 5).
//!
//! Two-pass grouping, mirroring `crate::group::group_movies`: pass 1 keys on
//! the mandatory discriminator (normalised show title), collecting candidate
//! years; pass 2 resolves one canonical year per group and writes it back.
//! Season/episode numbers are *not* part of this two-pass merge — unlike a
//! movie's year, which the filename may or may not carry, every episode
//! filename names its own season/episode directly (§3.2), so there is
//! nothing to fill in from siblings there. Only the *show*'s year is a
//! group-level property that individual episode filenames may omit.

use std::collections::HashMap;

use mm_core::identity::{Norm, ShowId};

use crate::tv_resolve::ResolvedEpisode;

/// A group of episodes sharing a show identity (i.e. all episodes of one
/// show, across every season, in this run).
#[derive(Debug, Clone)]
pub struct ShowGroup {
    pub id: ShowId,
    pub items: Vec<usize>, // indices into the planner's item list
}

/// Group resolved episodes by show identity.
pub fn group_shows(items: &[(usize, &ResolvedEpisode)]) -> Vec<ShowGroup> {
    // Pass 1: provisional groups by normalised show title.
    let mut by_title: HashMap<Norm, Vec<usize>> = HashMap::new();
    for (idx, e) in items {
        if let Some(title) = e.title.as_value() {
            let norm = Norm::from_display(title);
            by_title.entry(norm).or_default().push(*idx);
        }
    }

    // Pass 2: resolve canonical show year per title group and build ShowId.
    let mut groups = Vec::new();
    for (title_norm, indices) in by_title {
        let years: Vec<u16> = indices
            .iter()
            .filter_map(|i| {
                items
                    .iter()
                    .find(|(idx, _)| idx == i)
                    .and_then(|(_, e)| e.year.as_value().copied())
            })
            .collect();
        let canonical_year = pick_canonical_year(&years);

        let mut id = ShowId::new(title_norm);
        id.year = canonical_year;

        groups.push(ShowGroup { id, items: indices });
    }

    groups
}

fn pick_canonical_year(years: &[u16]) -> Option<u16> {
    if years.is_empty() {
        return None;
    }
    // Simple majority (ties broken by first-seen in iteration order), same
    // as `group::group_movies` — later phases could use source rank once
    // probe/tag data is wired in for TV (currently out of scope, see
    // `tv_plan` module docs).
    let mut counts: HashMap<u16, usize> = HashMap::new();
    for y in years {
        *counts.entry(*y).or_default() += 1;
    }
    counts.into_iter().max_by_key(|(_, c)| *c).map(|(y, _)| y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_core::provenance::{Confidence, Field, Source};

    fn resolved(title: &str, year: Option<u16>) -> ResolvedEpisode {
        ResolvedEpisode {
            title: Field::known(title.to_string(), Source::Filename, Confidence::Medium),
            year: match year {
                Some(y) => Field::known(y, Source::Filename, Confidence::Medium),
                None => Field::unknown(vec![]),
            },
            season: Field::known(1u16, Source::Filename, Confidence::High),
            episodes: Field::known(vec![1u16], Source::Filename, Confidence::High),
            ..Default::default()
        }
    }

    #[test]
    fn groups_episodes_missing_year_with_episodes_that_have_it() {
        let a = resolved("Show", Some(2011));
        let b = resolved("Show", None);
        let items = vec![(0usize, &a), (1usize, &b)];
        let groups = group_shows(&items);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].id.year, Some(2011));
        assert_eq!(groups[0].items.len(), 2);
    }

    #[test]
    fn different_titles_are_different_groups() {
        let a = resolved("Show One", Some(2011));
        let b = resolved("Show Two", Some(2015));
        let items = vec![(0usize, &a), (1usize, &b)];
        let groups = group_shows(&items);
        assert_eq!(groups.len(), 2);
    }
}
