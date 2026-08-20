//! TV resolve stage (§5.2, Phase 5).
//!
//! Mirrors `crate::resolve::ResolvedMovie`/`resolve_movie` in shape. Probe/tag
//! merging is out of scope for this pass (filename-only, like the movie
//! pipeline's own Phase 2 starting point) — see `tv_plan` module docs for the
//! flagged follow-up.

use mm_core::Field;
use mm_core::config::Config;
use mm_core::plan::{FieldName, Readiness};

use mm_parse::ParsedEpisode;

/// A TV episode with resolved fields ready for routing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedEpisode {
    /// Show title.
    pub title: Field<String>,
    /// Show year, only present to disambiguate two shows sharing a title.
    pub year: Field<u16>,
    pub season: Field<u16>,
    pub episodes: Field<Vec<u16>>,
    pub episode_title: Field<String>,
    pub resolution: Field<String>,
    pub source: Field<String>,
    pub video_codec: Field<String>,
    pub audio_format: Field<String>,
    pub hdr: Field<String>,
    pub copy: Field<u16>,
    /// Carried over from `ParsedEpisode::ambiguous` (§3.2: cross-season
    /// ranges, anime absolute numbering) — a distinct signal from "missing",
    /// gated into `Readiness::Ambiguous` rather than `NeedsReview`.
    pub ambiguous: bool,
}

impl ResolvedEpisode {
    pub fn from_parsed(p: &ParsedEpisode) -> Self {
        ResolvedEpisode {
            title: p.title.clone(),
            year: p.year.clone(),
            season: p.season.clone(),
            episodes: p.episodes.clone(),
            episode_title: p.episode_title.clone(),
            resolution: p.resolution.clone(),
            source: p.source.clone(),
            video_codec: p.video_codec.clone(),
            audio_format: p.audio_format.clone(),
            hdr: p.hdr.clone(),
            copy: p.copy.clone(),
            ambiguous: p.ambiguous,
        }
    }

    /// Determine readiness based on required fields (§5.2: `title`, `season`,
    /// `episodes` — `year` is optional for TV per `require_year_for_tv`) and
    /// minimum confidence.
    pub fn readiness(&self, cfg: &Config) -> Readiness {
        // An ambiguous marker (cross-season range, anime absolute numbering)
        // is a distinct finding from "missing" — it was recognised but could
        // legitimately mean more than one thing, so it is reported rather
        // than guessed (§3.2, §24).
        if self.ambiguous {
            return Readiness::Ambiguous {
                interpretations: vec![
                    "season/episode marker was ambiguous (e.g. cross-season range or absolute \
                     numbering) — confirm season and episode manually"
                        .to_string(),
                ],
            };
        }

        let mut missing = Vec::new();
        let mut reasons = Vec::new();

        if !self.title.is_known() {
            missing.push(FieldName::Title);
            reasons.push("title could not be determined".into());
        }
        if !self.season.is_known() {
            missing.push(FieldName::Season);
            reasons.push("season could not be determined".into());
        }
        match self.episodes.as_value() {
            Some(v) if !v.is_empty() => {}
            _ => {
                missing.push(FieldName::Episodes);
                reasons.push("episode number could not be determined".into());
            }
        }
        if cfg.behaviour.require_year_for_tv && !self.year.is_known() {
            missing.push(FieldName::Year);
            reasons.push("year is required for tv".into());
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
        [self.title.confidence(), self.season.confidence()]
            .into_iter()
            .flatten()
            .all(|c| c.meets(min))
    }
}

/// Resolve a parsed episode from filename candidates. Container/tag probing
/// is not wired in for Phase 5 (see `tv_plan` module docs) — this mirrors
/// `resolve::resolve_movie`'s own Phase-2 starting point.
pub fn resolve_episode(parsed: &ParsedEpisode, _cfg: &Config) -> ResolvedEpisode {
    ResolvedEpisode::from_parsed(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_core::provenance::{Confidence, Source};

    fn known<T>(v: T, c: Confidence) -> Field<T> {
        Field::known(v, Source::Filename, c)
    }

    #[test]
    fn ready_when_title_season_episodes_known() {
        let r = ResolvedEpisode {
            title: known("Show".to_string(), Confidence::Medium),
            season: known(1u16, Confidence::High),
            episodes: known(vec![2u16], Confidence::High),
            ..Default::default()
        };
        let cfg = Config::default();
        assert_eq!(r.readiness(&cfg), Readiness::Ready);
    }

    #[test]
    fn missing_episodes_needs_review() {
        let r = ResolvedEpisode {
            title: known("Show".to_string(), Confidence::Medium),
            season: known(1u16, Confidence::High),
            ..Default::default()
        };
        let cfg = Config::default();
        match r.readiness(&cfg) {
            Readiness::NeedsReview { missing, .. } => {
                assert!(missing.contains(&FieldName::Episodes));
            }
            other => panic!("expected NeedsReview, got {other:?}"),
        }
    }

    #[test]
    fn ambiguous_marker_is_ambiguous_readiness_not_needs_review() {
        let r = ResolvedEpisode {
            title: known("Show".to_string(), Confidence::Medium),
            ambiguous: true,
            ..Default::default()
        };
        let cfg = Config::default();
        assert!(matches!(r.readiness(&cfg), Readiness::Ambiguous { .. }));
    }

    #[test]
    fn year_not_required_by_default() {
        let r = ResolvedEpisode {
            title: known("Show".to_string(), Confidence::Medium),
            season: known(1u16, Confidence::High),
            episodes: known(vec![1u16], Confidence::High),
            ..Default::default()
        };
        let cfg = Config::default();
        assert!(!cfg.behaviour.require_year_for_tv);
        assert_eq!(r.readiness(&cfg), Readiness::Ready);
    }
}
