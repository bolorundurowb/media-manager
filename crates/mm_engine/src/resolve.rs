//! Resolve stage (§5.2).
//!
//! Merge candidates per field via the per-field source-preference table. Phase
//! 2 only has filename-sourced candidates; probing (Phase 4) will add more.

use mm_core::Field;
use mm_core::config::Config;
use mm_core::plan::{FieldName, Readiness};

use mm_parse::ParsedMovie;

/// A movie with resolved fields ready for routing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedMovie {
    pub title: Field<String>,
    pub year: Field<u16>,
    pub edition: Field<String>,
    pub resolution: Field<String>,
    pub source: Field<String>,
    pub video_codec: Field<String>,
    pub audio_format: Field<String>,
    pub hdr: Field<String>,
}

impl ResolvedMovie {
    pub fn from_parsed(p: &ParsedMovie) -> Self {
        ResolvedMovie {
            title: p.title.clone(),
            year: p.year.clone(),
            edition: p.edition.clone(),
            resolution: p.resolution.clone(),
            source: p.source.clone(),
            video_codec: p.video_codec.clone(),
            audio_format: p.audio_format.clone(),
            hdr: p.hdr.clone(),
        }
    }

    /// Determine readiness based on required fields and minimum confidence.
    pub fn readiness(&self, cfg: &Config) -> Readiness {
        let mut missing = Vec::new();
        let mut reasons = Vec::new();

        if !self.title.is_known() {
            missing.push(FieldName::Title);
            reasons.push("title could not be determined".into());
        }
        if cfg.behaviour.require_year_for_movies && !self.year.is_known() {
            missing.push(FieldName::Year);
            reasons.push("year is required for movies".into());
        }
        if !self.meets_min_confidence(cfg) {
            reasons.push("confidence below configured minimum".into());
        }

        if missing.is_empty() && reasons.is_empty() {
            Readiness::Ready
        } else {
            Readiness::NeedsReview { missing, reasons }
        }
    }

    fn meets_min_confidence(&self, cfg: &Config) -> bool {
        let min = cfg.behaviour.min_confidence;
        [self.title.confidence(), self.year.confidence()]
            .into_iter()
            .flatten()
            .all(|c| c.meets(min))
    }
}

/// Resolve a parsed movie. In Phase 2 this is a no-op merge (only filename
/// candidates); later phases merge embedded/container/provider candidates.
pub fn resolve_movie(parsed: &ParsedMovie, _cfg: &Config) -> ResolvedMovie {
    ResolvedMovie::from_parsed(parsed)
}
