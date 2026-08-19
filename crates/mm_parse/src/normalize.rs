//! Normalisation (§3.1 step 1).
//!
//! Strip the extension, unify `.`/`_`/`+` separators to spaces, collapse
//! whitespace. Bracketed and parenthesised spans are *not* collapsed away —
//! the year extractor needs to see whether a candidate sits inside `(...)` or
//! `[...]` (§3.2: "parenthesised/bracketed year wins over bare").

/// Strip a plausible file extension (short, alphanumeric, not the whole
/// name). Not a general "does this look like a media file" check — classify
/// already answered that; this only avoids feeding ".mkv" itself into the
/// extractor pipeline as a trailing token.
pub fn strip_extension(filename: &str) -> &str {
    match filename.rfind('.') {
        Some(i) if i > 0 && filename.len() - i <= 6 && filename.len() - i > 1 => {
            let ext = &filename[i + 1..];
            if ext.chars().all(|c| c.is_ascii_alphanumeric()) {
                &filename[..i]
            } else {
                filename
            }
        }
        _ => filename,
    }
}

/// Unify separators and collapse whitespace runs. Brackets/parens/other
/// punctuation are left untouched (extractors handle those explicitly).
pub fn unify_separators(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        let mapped = match ch {
            '.' | '_' | '+' => ' ',
            other => other,
        };
        if mapped == ' ' || mapped.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(mapped);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

/// Normalise a filename stem ready for extractor consumption.
pub fn normalize_stem(filename: &str) -> String {
    unify_separators(strip_extension(filename))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_short_extension_only() {
        assert_eq!(strip_extension("Movie.2010.mkv"), "Movie.2010");
        // A trailing short alphanumeric run after a dot is indistinguishable
        // from a real extension by this heuristic alone — mm-parse has no
        // config/extension-list dependency (§0: "no I/O deps at all"), so it
        // cannot do better than "short and alphanumeric" here. This is a
        // known, accepted imprecision, not a target for false-positive-proofing.
        assert_eq!(strip_extension("Movie.Name.Without.Ext"), "Movie.Name.Without");
        assert_eq!(
            strip_extension("Movie.Name.With.A.Long.Suffix.Indeed"),
            "Movie.Name.With.A.Long.Suffix.Indeed"
        );
        assert_eq!(strip_extension("noext"), "noext");
        assert_eq!(strip_extension(".hidden"), ".hidden");
    }

    #[test]
    fn unifies_and_collapses() {
        assert_eq!(unify_separators("Movie.Title__2010"), "Movie Title 2010");
        assert_eq!(unify_separators("A  B   C"), "A B C");
    }

    #[test]
    fn keeps_brackets_and_parens() {
        assert_eq!(
            normalize_stem("Movie.Title.(2010).[YTS.MX].mkv"),
            "Movie Title (2010) [YTS MX]"
        );
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        for s in ["", ".", "..", "...", "🎬🎬🎬.mkv", "a\u{0}b.mkv"] {
            let _ = normalize_stem(s);
        }
    }
}
