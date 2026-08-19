//! Property tests (§3.3, §3.4).
//!
//! Two independent laws:
//!
//! - **Field-level round-trip** (§3.3): for field sets the router would
//!   consider ready, `parse(render(F)).fields_relevant_to_naming() == F`.
//!   The law is stated over *fields*, not strings — see §3.3's own
//!   counterexample for why a string-level law is too weak to catch a wrong
//!   parse hiding behind an identical re-render.
//! - **Non-panic** (§3.4): the parser terminates without panicking on any
//!   `String`, however adversarial.

use std::collections::HashMap;

use mm_core::Template;
use mm_core::template::ValueSource;
use mm_parse::{ParseOptions, parse_movie_filename};
use proptest::prelude::*;

struct FieldMap(HashMap<String, String>);

impl ValueSource for FieldMap {
    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}

/// Title strings built from letters and single spaces only — no digits (so
/// nothing accidentally looks like a year or episode marker) — filtered to
/// exclude any word that collides with a vocabulary trigger (so nothing
/// accidentally looks like a resolution/source/edition/language tag). This
/// is what makes the law testable at all: a title that syntactically *is* a
/// reserved word is a genuine, spec-acknowledged ambiguity (§3.3, §24), not
/// a case this law promises to hold for.
fn title_strategy() -> impl Strategy<Value = String> {
    "[A-Z][a-z]{1,9}( [A-Z][a-z]{1,9}){0,3}".prop_filter("no reserved-word collision", |s| {
        // The vocab-driven categories, plus the episode-marker "special"
        // words, which aren't part of the vocab TOML (they're a structural
        // regex, not a tag lookup) but are exactly as reserved in practice.
        let mut reserved = mm_parse::vocab::all_reserved_words();
        reserved.extend(["special", "sp", "ova"].map(str::to_string));
        let lower = format!(" {} ", s.to_lowercase());
        !reserved
            .iter()
            .any(|w| lower.contains(&format!(" {} ", w.to_lowercase())))
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn field_level_round_trip_title_and_year(
        title in title_strategy(),
        year in prop::option::of(1888u16..=2027u16),
    ) {
        let template = Template::parse("{title}[ ({year})]").unwrap();
        let mut values = HashMap::new();
        values.insert("title".to_string(), title.clone());
        if let Some(y) = year {
            values.insert("year".to_string(), y.to_string());
        }
        let rendered = template.render(&FieldMap(values));
        let filename = format!("{rendered}.mkv");

        let opts = ParseOptions { min_year: 1888, max_year: 2027 };
        let parsed = parse_movie_filename(&filename, &opts);

        prop_assert_eq!(parsed.title.as_value().map(String::as_str), Some(title.as_str()));
        prop_assert_eq!(parsed.year.as_value().copied(), year);
    }

    #[test]
    fn never_panics_on_arbitrary_strings(s in ".{0,10000}") {
        let opts = ParseOptions::default();
        let _ = parse_movie_filename(&s, &opts);
    }

    #[test]
    fn never_panics_on_control_and_combining_chars(
        s in "[\\x00-\\x1f\\u{0300}-\\u{036f}\\u{200e}\\u{200f}a-zA-Z0-9 ]{0,500}"
    ) {
        let opts = ParseOptions::default();
        let _ = parse_movie_filename(&s, &opts);
    }
}
