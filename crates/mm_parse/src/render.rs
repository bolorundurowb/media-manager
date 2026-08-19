//! Render parsed fields back to a filename fragment (§3.3 round-trip tests).

use mm_core::Field;

use crate::model::ParsedMovie;

/// Render a movie to a canonical filename stem.
pub fn render_movie(m: &ParsedMovie) -> String {
    let mut out = String::new();
    push_field(&mut out, "", &m.title);
    if let Some(y) = m.year.as_value() {
        out.push_str(&format!(" ({})", y));
    }
    push_opt(&mut out, " - ", &m.edition);
    push_opt(&mut out, " - ", &m.resolution);
    push_opt(&mut out, " - ", &m.source);
    push_opt(&mut out, " - ", &m.video_codec);
    push_opt(&mut out, " - ", &m.audio_format);
    push_opt(&mut out, " - ", &m.hdr);
    out
}

fn push_field(out: &mut String, prefix: &str, f: &Field<String>) {
    if let Some(v) = f.as_value() {
        out.push_str(prefix);
        out.push_str(v);
    }
}

fn push_opt(out: &mut String, prefix: &str, f: &Field<String>) {
    if let Some(v) = f.as_value() {
        out.push_str(prefix);
        out.push_str(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_core::{Confidence, Source};

    use crate::model::known;

    #[test]
    fn renders_movie() {
        let m = ParsedMovie {
            title: known("Inception".into(), Source::Filename, Confidence::Medium),
            year: known(2010, Source::Filename, Confidence::High),
            resolution: known("1080p".into(), Source::Filename, Confidence::Medium),
            source: known("BluRay".into(), Source::Filename, Confidence::Medium),
            video_codec: known("x264".into(), Source::Filename, Confidence::Medium),
            ..Default::default()
        };
        assert_eq!(render_movie(&m), "Inception (2010) - 1080p - BluRay - x264");
    }
}
