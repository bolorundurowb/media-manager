//! Resolve stage (§5.2).
//!
//! Merge candidates per field via the per-field source-preference table.
//! Filename fields are the baseline; [`merge_field`] folds in container
//! (and later tag/provider) candidates using [`mm_core::identity::pick_best`].

use mm_core::Field;
use mm_core::config::Config;
use mm_core::identity::SourcePreference;
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
    pub copy: Field<u16>,
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
            copy: p.copy.clone(),
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

/// Resolve a parsed movie from filename candidates. Probe/tag fields are
/// folded in afterwards via [`merge_field`].
pub fn resolve_movie(parsed: &ParsedMovie, _cfg: &Config) -> ResolvedMovie {
    ResolvedMovie::from_parsed(parsed)
}

/// Keep the better of `current` and `candidate` per the source-preference table.
pub fn merge_field<T: Clone>(
    current: &Field<T>,
    candidate: Field<T>,
    prefs: &SourcePreference,
) -> Field<T> {
    mm_core::identity::pick_best(current, &candidate, prefs).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_core::identity::SourcePreference;
    use mm_core::provenance::{Confidence, Source};

    #[test]
    fn container_header_beats_filename_resolution() {
        let prefs = SourcePreference::conservative_default();
        let filename = Field::known("720p".into(), Source::Filename, Confidence::High);
        let container = Field::known("1080p".into(), Source::ContainerHeader, Confidence::High);
        let best = merge_field(&filename, container, &prefs);
        assert_eq!(best.as_value().map(String::as_str), Some("1080p"));
        assert_eq!(best.source(), Some(Source::ContainerHeader));
    }
}
