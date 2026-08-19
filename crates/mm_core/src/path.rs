//! Path layer (§2.4).
//!
//! One module owns every platform path concern. Sanitisation is unconditional
//! and specifically not conditioned on whether the syscall path is verbatim.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::volume::ComponentLimit;

/// The configurable character substitution map (§2.4). `/` and `\` are **not**
/// configurable — always replaced. `:` → ` -`, `?` → `` by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SanitiseMap(pub HashMap<char, String>);

impl Default for SanitiseMap {
    fn default() -> Self {
        let mut m = HashMap::new();
        // `:` → " -" (so "Vikings: S01" becomes "Vikings - S01")
        m.insert(':', " -".to_string());
        // `?` → "" (so "What? No" becomes "What No")
        m.insert('?', "".to_string());
        // angle brackets, quotes, pipes, asterisks
        m.insert('<', "".to_string());
        m.insert('>', "".to_string());
        m.insert('"', "'".to_string());
        m.insert('|', "-".to_string());
        m.insert('*', "".to_string());
        // slash/backslash — always replaced, even if user redefines the map.
        m.insert('/', "-".to_string());
        m.insert('\\', "-".to_string());
        SanitiseMap(m)
    }
}

/// Sanitise a single path component (no slashes) per §2.4.
///
/// Returns `None` if the result is empty — the caller must treat an empty name
/// as a hard `NeedsReview`, never a fallback name.
pub fn sanitise_component(name: &str, map: &SanitiseMap, limit: ComponentLimit) -> Option<String> {
    // 1. NFC for storage and comparison.
    let mut s: String = name.chars().nfc().collect();

    // 2. Replace forbidden chars and control chars.
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if (ch as u32) < 0x20 {
            // control char 0x00–0x1F — drop
            continue;
        }
        if let Some(repl) = map.0.get(&ch) {
            out.push_str(repl);
        } else {
            out.push(ch);
        }
    }
    s = out;

    // 3. Collapse runs of whitespace.
    s = collapse_whitespace(&s);

    // 4. Strip trailing dots and spaces (applied *before* reserved-name check).
    s = s.trim_end_matches(['.', ' ']).to_string();
    s = s.trim_start_matches(' ').to_string();

    // 5. Reserved device-name check (Windows). The rule is the name up to the
    //    first period, case-insensitively. Applied after trailing stripping so
    //    `CON .txt` and `CON.2010.mkv` are both caught. The suffix goes right
    //    after the reserved stem — `CON.mkv` -> `CON_.mkv` — not at the end of
    //    the whole component, or `CON.2010.mkv` would render `CON.2010.mkv_`
    //    and still collide with the reserved name up to its first period.
    if is_reserved_device_name(&s) {
        insert_reserved_suffix(&mut s);
    }

    // 6. Component length cap on a grapheme-cluster boundary.
    if !limit.fits(&s) {
        s = limit.truncate(&s);
        // re-strip trailing dots after truncation
        s = s.trim_end_matches(['.', ' ']).to_string();
        // reserved-name check again post-truncation (paranoia)
        if is_reserved_device_name(&s) {
            insert_reserved_suffix(&mut s);
        }
    }

    // 7. Trim once more in case truncation left a dangling separator run.
    let s = s.trim_matches(|c: char| c == ' ' || c == '.').to_string();

    if s.is_empty() { None } else { Some(s) }
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out
}

/// The reserved Windows device names (§2.4).
pub fn is_reserved_device_name(name: &str) -> bool {
    // Take the stem up to the first period.
    let stem = match name.find('.') {
        Some(i) => &name[..i],
        None => name,
    };
    let lower = stem.to_ascii_lowercase();
    const FIXED: &[&str] = &["con", "prn", "aux", "nul", "conin$", "conout$", "clock$"];
    if FIXED.contains(&lower.as_str()) {
        return true;
    }
    // COM0-9 / LPT0-9 are always ASCII; guard on that before `split_at(4)` —
    // a non-ASCII stem of byte length >= 4 (e.g. a CJK title) can have its
    // 4-byte boundary fall inside a multi-byte character and panic.
    if lower.is_ascii() && lower.len() == 4 {
        let (pfx, num) = lower.split_at(3);
        if matches!(pfx, "com" | "lpt")
            && matches!(
                num,
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"
            )
        {
            return true;
        }
    }
    false
}

/// Insert the reserved-name suffix (`_`) right after the reserved stem — the
/// portion up to the first period — rather than at the end of the whole
/// component. `CON.mkv` -> `CON_.mkv`, not `CON.mkv_` (which would leave the
/// reserved stem `CON` intact and still unusable on Windows).
fn insert_reserved_suffix(s: &mut String) {
    let cut = s.find('.').unwrap_or(s.len());
    s.insert(cut, '_');
}

/// Build a destination path from sanitised components.
pub fn join_components(root: &Path, parts: &[String]) -> PathBuf {
    let mut p = root.to_path_buf();
    for part in parts {
        p.push(part);
    }
    p
}

/// Normalise a relative path: drop `.` components, collapse `..` lexically
/// (without touching the fs). Returns `None` if the path escapes root.
pub fn normalise_relative(rel: &Path) -> Option<PathBuf> {
    let mut out = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s.to_owned()),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    let mut p = PathBuf::new();
    for c in out {
        p.push(c);
    }
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::volume::VolumeSemantics;

    #[test]
    fn sanitises_colon_and_question() {
        let map = SanitiseMap::default();
        let s = sanitise_component("Movie: Title?", &map, VolumeSemantics::ntfs().max_component)
            .unwrap();
        assert_eq!(s, "Movie - Title");
    }

    #[test]
    fn strips_trailing_dots_and_spaces() {
        let map = SanitiseMap::default();
        let s = sanitise_component("Movie .", &map, VolumeSemantics::ntfs().max_component).unwrap();
        assert_eq!(s, "Movie");
    }

    #[test]
    fn reserved_con_with_extension() {
        assert!(is_reserved_device_name("CON.2010.mkv"));
        assert!(is_reserved_device_name("con"));
        assert!(is_reserved_device_name("PRN.txt"));
        assert!(!is_reserved_device_name("Connect"));
        assert!(is_reserved_device_name("COM1"));
        assert!(!is_reserved_device_name("COM10"));
    }

    #[test]
    fn reserved_name_gets_suffix() {
        let map = SanitiseMap::default();
        let s = sanitise_component("CON.mkv", &map, VolumeSemantics::ntfs().max_component).unwrap();
        assert_eq!(s, "CON_.mkv");
    }

    #[test]
    fn empty_after_sanitise_is_none() {
        let map = SanitiseMap::default();
        // `?` and `*` both map to "" by default (§2.4); `:` maps to " -" and
        // so deliberately isn't included here — a name that's *only* a
        // separator a user put there on purpose (`" -"`) is not the same
        // failure as one that sanitises away to nothing.
        let s = sanitise_component("???***", &map, VolumeSemantics::ntfs().max_component);
        assert_eq!(s, None);
    }

    #[test]
    fn truncates_on_byte_limit() {
        let map = SanitiseMap::default();
        let long = "あ".repeat(200); // 3 bytes each = 600 bytes
        let s = sanitise_component(&long, &map, ComponentLimit::Bytes(255)).unwrap();
        // grapheme-aware: 85 × 3 = 255 bytes
        assert_eq!(s.len(), 255);
    }

    #[test]
    fn normalises_relative_drops_dotdot() {
        let r = normalise_relative(Path::new("a/./b/../c")).unwrap();
        assert_eq!(r, PathBuf::from("a/c"));
    }
}
