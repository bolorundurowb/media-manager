//! Corpus-driven TV parser test (§11, §22.3, §3.2).
//!
//! Reads `testdata/names/tv.toml` and asserts the parser extracts every field
//! each case lists. Modelled directly on `tests/corpus.rs` (movies) — a case
//! only asserts the fields it lists, so a case can focus on just the one
//! thing it's meant to demonstrate (e.g. a multi-episode case only asserts
//! `season`/`episodes`).

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    title: Option<String>,
    year: Option<u16>,
    season: Option<u16>,
    episodes: Option<Vec<u16>>,
    episode_title: Option<String>,
    resolution: Option<String>,
    source: Option<String>,
    video_codec: Option<String>,
    audio_format: Option<String>,
    hdr: Option<String>,
    release_group: Option<String>,
    #[serde(default)]
    ambiguous: bool,
}

#[derive(Debug, Deserialize)]
struct Corpus {
    case: Vec<Case>,
}

fn corpus_path() -> String {
    format!("{}/../../testdata/names/tv.toml", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn tv_corpus_matches_expected_fields() {
    let path = corpus_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("corpus file must exist at {path}: {e}"));
    let corpus: Corpus = toml::from_str(&raw).expect("corpus must parse as valid TOML");

    assert!(
        corpus.case.len() >= 20,
        "tv corpus must have >= 20 cases, has {}",
        corpus.case.len()
    );

    let opts = mm_parse::ParseOptions {
        min_year: 1888,
        max_year: 2027,
    };

    let mut failures: Vec<String> = Vec::new();
    for c in &corpus.case {
        let p = mm_parse::parse_episode_filename(&c.name, &opts);
        let mut mismatches: Vec<String> = Vec::new();

        macro_rules! check {
            ($expected:expr, $field:expr, $label:literal) => {
                if let Some(expected) = &$expected {
                    let actual = $field.as_value().map(|s| s.to_string());
                    if actual.as_deref() != Some(expected.as_str()) {
                        mismatches.push(format!(
                            "{}: expected {:?}, got {:?}",
                            $label, expected, actual
                        ));
                    }
                }
            };
        }

        check!(c.title, p.title, "title");
        check!(c.episode_title, p.episode_title, "episode_title");
        check!(c.resolution, p.resolution, "resolution");
        check!(c.source, p.source, "source");
        check!(c.video_codec, p.video_codec, "video_codec");
        check!(c.audio_format, p.audio_format, "audio_format");
        check!(c.hdr, p.hdr, "hdr");
        check!(c.release_group, p.release_group, "release_group");

        if let Some(year) = c.year {
            let actual = p.year.as_value().copied();
            if actual != Some(year) {
                mismatches.push(format!("year: expected {year:?}, got {actual:?}"));
            }
        }
        if let Some(season) = c.season {
            let actual = p.season.as_value().copied();
            if actual != Some(season) {
                mismatches.push(format!("season: expected {season:?}, got {actual:?}"));
            }
        }
        if let Some(episodes) = &c.episodes {
            let actual = p.episodes.as_value();
            if actual != Some(episodes) {
                mismatches.push(format!("episodes: expected {episodes:?}, got {actual:?}"));
            }
        }
        if c.ambiguous && !p.ambiguous {
            mismatches.push("expected ambiguous = true".to_string());
        }

        if !mismatches.is_empty() {
            failures.push(format!("{}: {}", c.name, mismatches.join("; ")));
        }
    }

    assert!(
        failures.is_empty(),
        "{} / {} corpus cases failed:\n{}",
        failures.len(),
        corpus.case.len(),
        failures.join("\n")
    );
}
