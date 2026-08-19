//! Small typed template engine (§5.5, §3.3).
//!
//! Not a general template engine: a validated placeholder whitelist plus
//! optional-segment brackets `[...]`. Validation at config load rejects an
//! unbracketed placeholder bound to an optional field, so a corrupted library
//! becomes a startup error rather than a runtime bug.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::CoreError;

/// A placeholder name known to the template grammar.
pub const KNOWN_PLACEHOLDERS: &[&str] = &[
    "title",
    "year",
    "edition",
    "resolution",
    "hdr",
    "source",
    "video_codec",
    "audio_format",
    "discriminators",
    "season",
    "episode_code",
    "episode_title",
    "album_artist",
    "album",
    "disc",
    "track",
    "track_artist",
    "language",
    "flags",
];

/// Which placeholders are optional (may be absent) — bracketing them is
/// mandatory (§5.5). A bare optional placeholder renders a leading separator
/// when absent, which is the bug validation prevents.
///
/// `discriminators` and `language` are *not* optional: `discriminators`
/// contains its own separators internally and may render empty harmlessly;
/// `language` always renders (defaulting to `und` for subtitles).
pub fn is_optional_placeholder(name: &str) -> bool {
    matches!(
        name,
        "year"
            | "edition"
            | "resolution"
            | "hdr"
            | "source"
            | "video_codec"
            | "audio_format"
            | "episode_title"
            | "track"
            | "track_artist"
            | "flags"
    )
}

/// A compiled template: a sequence of literal text and placeholder segments,
/// where each segment may be optional (bracketed) or required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub raw: String,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Literal(String),
    /// A single placeholder that must be present.
    Required(String),
    /// A group of literals/placeholders that is rendered only when *all* its
    /// placeholders are present.
    Optional(Vec<Segment>),
}

impl Serialize for Template {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for Template {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Template::parse(&raw).map_err(serde::de::Error::custom)
    }
}

impl Default for Template {
    fn default() -> Self {
        Template::parse("{title}").expect("default template must parse")
    }
}

impl Template {
    /// Parse and validate a template string.
    pub fn parse(raw: &str) -> Result<Self, CoreError> {
        let segments = parse_segments(raw)?;
        validate(&segments)?;
        Ok(Template {
            raw: raw.to_string(),
            segments,
        })
    }

    /// Render the template against a value source.
    pub fn render(&self, values: &dyn ValueSource) -> String {
        let mut out = String::new();
        render_segments(&self.segments, values, &mut out);
        out
    }
}

/// A value source for rendering — the planner supplies a concrete impl per
/// item.
pub trait ValueSource {
    fn get(&self, name: &str) -> Option<String>;
}

fn render_segments(segs: &[Segment], values: &dyn ValueSource, out: &mut String) {
    for seg in segs {
        match seg {
            Segment::Literal(s) => out.push_str(s),
            Segment::Required(name) => {
                if let Some(v) = values.get(name) {
                    out.push_str(&v);
                }
            }
            Segment::Optional(inner) => {
                if optional_present(inner, values) {
                    render_segments(inner, values, out);
                }
            }
        }
    }
}

/// `true` if every placeholder in `segs` is present in `values`.
fn optional_present(segs: &[Segment], values: &dyn ValueSource) -> bool {
    segs.iter().all(|s| segment_satisfied(s, values))
}

fn segment_satisfied(seg: &Segment, values: &dyn ValueSource) -> bool {
    match seg {
        Segment::Literal(_) => true,
        Segment::Required(n) => values.get(n).is_some(),
        Segment::Optional(inner) => optional_present(inner, values),
    }
}

fn parse_segments(raw: &str) -> Result<Vec<Segment>, CoreError> {
    let mut segs = Vec::new();
    let mut buf = String::new();
    let mut chars = raw.char_indices().peekable();
    let bytes = raw.as_bytes();
    while let Some((i, c)) = chars.next() {
        if c == '[' {
            if !buf.is_empty() {
                segs.push(Segment::Literal(std::mem::take(&mut buf)));
            }
            // find matching ]
            let close = find_close(raw, i)?;
            let inner = &raw[i + 1..close];
            let inner_segs = parse_segments(inner)?;
            segs.push(Segment::Optional(inner_segs));
            // advance chars past close
            let consumed = close + 1;
            while let Some(&(j, _)) = chars.peek() {
                if j < consumed {
                    chars.next();
                } else {
                    break;
                }
            }
        } else if c == ']' {
            return Err(CoreError::InvalidTemplate(format!(
                "unmatched ']' at byte {i}"
            )));
        } else if c == '{' {
            if !buf.is_empty() {
                segs.push(Segment::Literal(std::mem::take(&mut buf)));
            }
            let close = raw[i..]
                .find('}')
                .ok_or_else(|| CoreError::InvalidTemplate(format!("unmatched '{{' at byte {i}")))?;
            let name_full = &raw[i + 1..i + close];
            let name = name_full.split(':').next().unwrap_or(name_full);
            if !KNOWN_PLACEHOLDERS.contains(&name) {
                return Err(CoreError::InvalidTemplate(format!(
                    "unknown placeholder '{{{name}}}'"
                )));
            }
            segs.push(Segment::Required(name.to_string()));
            let consumed = i + close + 1;
            while let Some(&(j, _)) = chars.peek() {
                if j < consumed {
                    chars.next();
                } else {
                    break;
                }
            }
            let _ = bytes; // keep bytes referenced
        } else {
            buf.push(c);
        }
    }
    if !buf.is_empty() {
        segs.push(Segment::Literal(buf));
    }
    Ok(segs)
}

fn find_close(raw: &str, open: usize) -> Result<usize, CoreError> {
    let mut depth = 0i32;
    let bytes = raw.as_bytes();
    for i in open..bytes.len() {
        match bytes[i] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            b'{' => {
                // skip to matching }
                let close = raw[i..].find('}').ok_or_else(|| {
                    CoreError::InvalidTemplate(format!("unmatched '{{' at byte {i}"))
                })?;
                let _ = close;
            }
            _ => {}
        }
    }
    Err(CoreError::InvalidTemplate(format!(
        "unmatched '[' at byte {open}"
    )))
}

fn validate(segs: &[Segment]) -> Result<(), CoreError> {
    validate_at(segs, true)
}

/// `top_level` is `true` only for segments not nested inside any `Optional`.
/// The "must be bracketed" rule (§5.5) is about *unbracketed* optional
/// placeholders specifically — a placeholder already inside `[...]` (at any
/// nesting depth) has satisfied the rule, so the check does not re-apply
/// once we've recursed into an `Optional`'s contents.
fn validate_at(segs: &[Segment], top_level: bool) -> Result<(), CoreError> {
    for seg in segs {
        match seg {
            Segment::Literal(_) => {}
            Segment::Required(name) => {
                if top_level && is_optional_placeholder(name) {
                    return Err(CoreError::InvalidTemplate(format!(
                        "placeholder '{{{name}}}' is optional and must be bracketed: [...]"
                    )));
                }
            }
            Segment::Optional(inner) => validate_at(inner, false)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Map(HashMap<String, String>);
    impl ValueSource for Map {
        fn get(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    #[test]
    fn parses_movie_template() {
        let t = Template::parse("{title}[ ({year})]").unwrap();
        let mut m = Map::default();
        m.0.insert("title".into(), "Inception".into());
        m.0.insert("year".into(), "2010".into());
        assert_eq!(t.render(&m), "Inception (2010)");
        m.0.remove("year");
        assert_eq!(t.render(&m), "Inception");
    }

    #[test]
    fn rejects_unbracketed_optional() {
        // year is optional; bare {year} must be rejected
        assert!(Template::parse("{title} {year}").is_err());
    }

    #[test]
    fn rejects_unknown_placeholder() {
        assert!(Template::parse("{notakeword}").is_err());
    }

    #[test]
    fn disc_separator_only_when_present() {
        let t = Template::parse("[{track:02} - ]{title}").unwrap();
        let mut m = Map::default();
        m.0.insert("title".into(), "Song".into());
        assert_eq!(t.render(&m), "Song");
        m.0.insert("track".into(), "01".into());
        assert_eq!(t.render(&m), "01 - Song");
    }
}
