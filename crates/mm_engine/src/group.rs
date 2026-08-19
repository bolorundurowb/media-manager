//! Group stage (§5.3, §2.2).
//!
//! Two-pass grouping: pass 1 keys on the mandatory discriminator (normalised
//! title), collecting candidate years; pass 2 resolves one canonical year per
//! group and writes it back; pass 3 keys on the complete `MovieId`.

use std::collections::HashMap;

use mm_core::identity::{MovieId, Norm};

use crate::resolve::ResolvedMovie;

/// A group of movies sharing an identity.
#[derive(Debug, Clone)]
pub struct MovieGroup {
    pub id: MovieId,
    pub items: Vec<usize>, // indices into the planner's item list
}

/// Group resolved movies by identity.
pub fn group_movies(items: &[(usize, &ResolvedMovie)]) -> Vec<MovieGroup> {
    // Pass 1: provisional groups by title.
    let mut by_title: HashMap<Norm, Vec<usize>> = HashMap::new();
    for (idx, m) in items {
        if let Some(title) = m.title.as_value() {
            let norm = Norm::from_display(title);
            by_title.entry(norm).or_default().push(*idx);
        }
    }

    // Pass 2: resolve canonical year per title group and build MovieId.
    let mut groups = Vec::new();
    for (title_norm, indices) in by_title {
        let years: Vec<u16> = indices
            .iter()
            .filter_map(|i| {
                items
                    .iter()
                    .find(|(idx, _)| idx == i)
                    .and_then(|(_, m)| m.year.as_value().copied())
            })
            .collect();
        let canonical_year = pick_canonical_year(&years);

        let mut id = MovieId::new(title_norm);
        id.year = canonical_year;

        groups.push(MovieGroup { id, items: indices });
    }

    groups
}

fn pick_canonical_year(years: &[u16]) -> Option<u16> {
    if years.is_empty() {
        return None;
    }
    // Simple majority / first for Phase 2. Later phases use source rank.
    let mut counts: HashMap<u16, usize> = HashMap::new();
    for y in years {
        *counts.entry(*y).or_default() += 1;
    }
    counts.into_iter().max_by_key(|(_, c)| *c).map(|(y, _)| y)
}
