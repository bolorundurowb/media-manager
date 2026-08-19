//! Classify stage (§2.3, §5).

use mm_core::classify::{FileClass, is_artwork_stem, is_metadata_ext};
use mm_core::config::Config;

use crate::scan::ScannedFile;

/// Classify every scanned file by extension and filename heuristic.
pub fn classify(files: &mut [ScannedFile], cfg: &Config) {
    for f in files.iter_mut() {
        f.class = classify_one(f, cfg);
    }
}

fn classify_one(f: &ScannedFile, cfg: &Config) -> FileClass {
    let ext = f
        .absolute
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let mut class = cfg.classify_ext(&ext);
    if class == FileClass::Unknown {
        if let Some(stem) = f.absolute.file_stem().and_then(|s| s.to_str()) {
            if is_artwork_stem(stem) {
                class = FileClass::Artwork;
            } else if is_metadata_ext(&ext) {
                class = FileClass::Metadata;
            }
        }
    }
    class
}
