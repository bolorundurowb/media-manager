//! Corpus-driven parser test (§11, §22.1).
//!
//! Reads `testdata/names/movies.toml` and asserts the parser extracts every
//! field each case lists. A case only asserts the fields it lists — this is
//! not a full-record comparison, so a case can focus on just the one thing
//! it's meant to demonstrate (e.g. the year-in-title tricky cases near the
//! bottom of the corpus file only assert `title`/`year`).

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    title: Option<String>,
    year: Option<u16>,
    resolution: Option<String>,
    source: Option<String>,
    video_codec: Option<String>,
    audio_format: Option<String>,
    edition: Option<String>,
    hdr: Option<String>,
    release_group: Option<String>,
    #[serde(default)]
    ambiguous_episode_like: bool,
}

#[derive(Debug, Deserialize)]
struct Corpus {
    case: Vec<Case>,
}

fn corpus_path() -> String {
    format!(
        "{}/../../testdata/names/movies.toml",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[test]
fn movie_corpus_matches_expected_fields() {
    let path = corpus_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("corpus file must exist at {path}: {e}"));
    let corpus: Corpus = toml::from_str(&raw).expect("corpus must parse as valid TOML");

    assert!(
        corpus.case.len() >= 10,
        "starter corpus must have >= 10 cases, has {} (full 150-case corpus is Phase 1 target)",
        corpus.case.len()
    );

    let opts = mm_parse::ParseOptions {
        min_year: 1888,
        max_year: 2027,
    };

    let mut failures: Vec<String> = Vec::new();
    for c in &corpus.case {
        let p = mm_parse::parse_movie_filename(&c.name, &opts);
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
        check!(c.resolution, p.resolution, "resolution");
        check!(c.source, p.source, "source");
        check!(c.video_codec, p.video_codec, "video_codec");
        check!(c.audio_format, p.audio_format, "audio_format");
        check!(c.edition, p.edition, "edition");
        check!(c.hdr, p.hdr, "hdr");
        check!(c.release_group, p.release_group, "release_group");

        if let Some(year) = c.year {
            let actual = p.year.as_value().copied();
            if actual != Some(year) {
                mismatches.push(format!("year: expected {year:?}, got {actual:?}"));
            }
        }
        if c.ambiguous_episode_like && !p.ambiguous_episode_like {
            mismatches.push("expected ambiguous_episode_like = true".to_string());
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
