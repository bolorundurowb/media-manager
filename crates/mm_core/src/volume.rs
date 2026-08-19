//! Volume semantics (§2.4).
//!
//! Case and normalisation sensitivity are queried, never probed by writing.
//! When detection fails, assume the conservative case (insensitive and
//! normalisation-insensitive): the conservative assumption over-merges
//! collision keys, producing a spurious *conflict report* (safe, visible)
//! rather than a missed collision (data loss).

use serde::{Deserialize, Serialize};

/// Per-target component length cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "limit")]
pub enum ComponentLimit {
    /// NTFS/HFS+ — 255 UTF-16 code units.
    Utf16Units(u32),
    /// ext4/XFS/APFS — 255 bytes.
    Bytes(u32),
}

impl ComponentLimit {
    /// `true` if `name` fits within this limit.
    pub fn fits(&self, name: &str) -> bool {
        match self {
            ComponentLimit::Utf16Units(n) => name.encode_utf16().count() as u32 <= *n,
            ComponentLimit::Bytes(n) => name.len() as u32 <= *n,
        }
    }

    /// Truncate `name` to fit on a grapheme-cluster boundary.
    pub fn truncate(&self, name: &str) -> String {
        use unicode_segmentation::UnicodeSegmentation;
        match self {
            ComponentLimit::Utf16Units(max) => {
                let mut units = 0u32;
                let mut out = String::with_capacity(name.len());
                for gc in name.graphemes(true) {
                    let g = gc.encode_utf16().count() as u32;
                    if units + g > *max {
                        break;
                    }
                    units += g;
                    out.push_str(gc);
                }
                out
            }
            ComponentLimit::Bytes(max) => {
                let mut bytes = 0u32;
                let mut out = String::with_capacity(name.len());
                for gc in name.graphemes(true) {
                    let g = gc.len() as u32;
                    if bytes + g > *max {
                        break;
                    }
                    bytes += g;
                    out.push_str(gc);
                }
                out
            }
        }
    }
}

/// The volume descriptor used to build collision keys (§5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeSemantics {
    pub case_sensitive: bool,
    pub normalisation_sensitive: bool,
    pub max_component: ComponentLimit,
    /// PATH_MAX on Linux, ~32767 via the long-path route on Windows.
    pub max_total: u32,
}

impl Default for VolumeSemantics {
    fn default() -> Self {
        VolumeSemantics::conservative()
    }
}

impl VolumeSemantics {
    /// Conservative default — assumed when detection fails.
    pub fn conservative() -> Self {
        VolumeSemantics {
            case_sensitive: false,
            normalisation_sensitive: false,
            max_component: ComponentLimit::Bytes(255),
            max_total: 4096,
        }
    }

    /// A case-sensitive, normalisation-sensitive volume (typical Linux ext4).
    pub fn sensitive_bytes() -> Self {
        VolumeSemantics {
            case_sensitive: true,
            normalisation_sensitive: true,
            max_component: ComponentLimit::Bytes(255),
            max_total: 4096,
        }
    }

    /// NTFS (Windows) defaults: case-insensitive, normalisation-insensitive,
    /// 255 UTF-16 code unit components, long paths.
    pub fn ntfs() -> Self {
        VolumeSemantics {
            case_sensitive: false,
            normalisation_sensitive: false,
            max_component: ComponentLimit::Utf16Units(255),
            max_total: 32_767,
        }
    }

    /// APFS (default since 10.13): normalisation-insensitive but
    /// normalisation-preserving — writing NFC is stable, so no rename loop.
    pub fn apfs() -> Self {
        VolumeSemantics {
            case_sensitive: false,
            normalisation_sensitive: false,
            max_component: ComponentLimit::Bytes(255),
            max_total: 4096,
        }
    }

    /// Build a collision key from a path string under these semantics.
    /// When insensitive, fold to lowercase; when normalisation-insensitive,
    /// fold to NFC.
    pub fn collision_key(&self, s: &str) -> String {
        use unicode_normalization::UnicodeNormalization;
        let s = if self.normalisation_sensitive {
            s.to_string()
        } else {
            s.chars().nfc().collect::<String>()
        };
        if self.case_sensitive {
            s
        } else {
            s.to_lowercase()
        }
    }
}

/// The no-replace strategy selected per volume (§2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoReplaceStrategy {
    /// Use a kernel no-replace rename primitive (Linux/Windows on every fs in
    /// scope; macOS on APFS/HFS+).
    Native,
    /// `create_new` exclusive-create reservation; always for cross-device
    /// moves and for macOS SMB/NFS/FAT.
    Reserve,
    /// Opt-in racy check-then-rename.
    CheckThenRename,
}

impl NoReplaceStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            NoReplaceStrategy::Native => "native",
            NoReplaceStrategy::Reserve => "reserve",
            NoReplaceStrategy::CheckThenRename => "check_then_rename",
        }
    }
}
