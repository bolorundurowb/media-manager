//! Tag vocabularies (§3.5).
//!
//! Compiled-in for Phase 2. Later phases move these to TOML so adding a
//! release tag needs no code change.

pub fn resolution_patterns() -> &'static [&'static str] {
    &[
        "4320p", "8K", "2160p", "4K", "1440p", "1080p", "720p", "576p", "480p",
    ]
}

pub fn source_patterns() -> &'static [&'static str] {
    &[
        "REMUX", "BluRay", "BDRip", "BRRip", "WEB-DL", "WEBDL", "WEB-Rip", "WEBRip", "HDRip",
        "DVDRip", "DVD-Rip", "HDTV", "PDTV", "VHSRip", "CAM", "TS", "TC", "SCR", "R5", "LD",
    ]
}

pub fn video_codec_patterns() -> &'static [&'static str] {
    &[
        "x265", "x264", "h265", "h264", "HEVC", "AVC", "XviD", "DivX", "AV1", "VP9",
    ]
}

pub fn audio_patterns() -> &'static [&'static str] {
    &[
        "Atmos",
        "TrueHD",
        "DTS-HD",
        "DTS-HD MA",
        "DTS",
        "DDP5.1",
        "DDP",
        "DD5.1",
        "E-AC3",
        "AC3",
        "AAC",
        "FLAC",
        "MP3",
        "Vorbis",
        "Opus",
    ]
}

pub fn hdr_patterns() -> &'static [&'static str] {
    &["HDR10+", "HDR10", "HDR", "Dolby Vision", "DV", "HLG", "SDR"]
}

pub fn edition_patterns() -> &'static [&'static str] {
    &[
        "Director's Cut",
        "Directors Cut",
        "Extended Cut",
        "Extended",
        "Theatrical Cut",
        "Theatrical",
        "Unrated",
        "Uncut",
        "Remastered",
        "Criterion",
        "IMAX",
        "Ultimate Edition",
        "Special Edition",
    ]
}

/// All words that have a reserved syntactic meaning in the movie parser.
/// Used by property tests to avoid generating titles that accidentally parse
/// as tags (§3.3, §3.4).
pub fn all_reserved_words() -> Vec<String> {
    let mut out = Vec::new();
    out.extend(resolution_patterns().iter().map(|s| s.to_string()));
    out.extend(source_patterns().iter().map(|s| s.to_string()));
    out.extend(video_codec_patterns().iter().map(|s| s.to_string()));
    out.extend(audio_patterns().iter().map(|s| s.to_string()));
    out.extend(hdr_patterns().iter().map(|s| s.to_string()));
    out.extend(edition_patterns().iter().map(|s| s.to_string()));
    out
}

/// Map a language token (ISO 639-1/639-2/B/T, English name, endonym) to ISO
/// 639-1. Returns `"und"` when unknown (§5.4).
pub fn normalise_language(token: &str) -> String {
    let t = token.to_ascii_lowercase();
    let code = match t.as_str() {
        "en" | "eng" | "english" => "en",
        "fr" | "fre" | "fra" | "french" => "fr",
        "es" | "spa" | "spanish" => "es",
        "de" | "ger" | "deu" | "german" => "de",
        "it" | "ita" | "italian" => "it",
        "pt" | "por" | "portuguese" => "pt",
        "ru" | "rus" | "russian" => "ru",
        "ja" | "jpn" | "japanese" => "ja",
        "zh" | "chi" | "zho" | "chinese" => "zh",
        "ko" | "kor" | "korean" => "ko",
        "nl" | "dut" | "nld" | "dutch" => "nl",
        "sv" | "swe" | "swedish" => "sv",
        "no" | "nor" | "norwegian" => "no",
        "da" | "dan" | "danish" => "da",
        "fi" | "fin" | "finnish" => "fi",
        "pl" | "pol" | "polish" => "pl",
        "ar" | "ara" | "arabic" => "ar",
        "hi" | "hin" | "hindi" => "hi",
        _ => "und",
    };
    code.to_string()
}
