//! DAR-corrected resolution banding.
//!
//! Labels key on **`max(width, height)` after display-aspect correction**, not
//! on height. Banding on height mislabels every scope-ratio encode: 1920×800
//! is a 1080p release (`max = 1920 ≥ 1700`), and a height table would send it
//! to 720p. 3840×1600 would land on 1440p.
//!
//! Display dimensions (`display_width` / `display_height`, or ISO-BMFF `tkhd`
//! vs sample-description dims) feed the band where both are present; pixel
//! dimensions otherwise. DVD example: 720×576 pixel and 1024×576 display →
//! use 1024×576, `max = 1024` → `576p`.

use crate::probe::VideoInfo;

/// Thresholds for [`label_resolution`]. Defaults match PLAN.md §4; later
/// phases can load these from config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionBands {
    /// `max(w, h) >= this` → `"4320p"`.
    pub band_4320p: u32,
    /// `max(w, h) >= this` → `"2160p"`.
    pub band_2160p: u32,
    /// `max(w, h) >= this` → `"1440p"`.
    pub band_1440p: u32,
    /// `max(w, h) >= this` → `"1080p"`.
    pub band_1080p: u32,
    /// `max(w, h) >= this` → `"720p"`.
    pub band_720p: u32,
    /// `max(w, h) >= this` → `"576p"`.
    pub band_576p: u32,
    /// `max(w, h) >= this` → `"480p"`.
    pub band_480p: u32,
}

impl Default for ResolutionBands {
    fn default() -> Self {
        ResolutionBands {
            band_4320p: 7000,
            band_2160p: 3000,
            band_1440p: 2200,
            band_1080p: 1700,
            band_720p: 1100,
            band_576p: 900,
            band_480p: 700,
        }
    }
}

/// Display dimensions if both are present and non-zero; otherwise pixel dims.
pub fn corrected_dims(info: &VideoInfo) -> (u32, u32) {
    match (info.display_width, info.display_height) {
        (Some(dw), Some(dh)) if dw > 0 && dh > 0 => (dw, dh),
        _ => (info.pixel_width, info.pixel_height),
    }
}

/// Resolution label for `info` using default [`ResolutionBands`].
///
/// The string is what the engine stores as `Field<String>` with
/// `Source::ContainerHeader` and `Confidence::High`.
pub fn label_resolution(info: &VideoInfo) -> Option<String> {
    label_resolution_with(info, &ResolutionBands::default())
}

/// Resolution label using caller-supplied thresholds.
pub fn label_resolution_with(info: &VideoInfo, bands: &ResolutionBands) -> Option<String> {
    let (w, h) = corrected_dims(info);
    if w == 0 || h == 0 {
        return None;
    }
    let max = w.max(h);
    if max >= bands.band_4320p {
        Some("4320p".into())
    } else if max >= bands.band_2160p {
        Some("2160p".into())
    } else if max >= bands.band_1440p {
        Some("1440p".into())
    } else if max >= bands.band_1080p {
        Some("1080p".into())
    } else if max >= bands.band_720p {
        Some("720p".into())
    } else if max >= bands.band_576p {
        Some("576p".into())
    } else if max >= bands.band_480p {
        Some("480p".into())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(pw: u32, ph: u32, dw: Option<u32>, dh: Option<u32>) -> VideoInfo {
        VideoInfo {
            pixel_width: pw,
            pixel_height: ph,
            display_width: dw,
            display_height: dh,
            codec: None,
        }
    }

    #[test]
    fn scope_1920x800_is_1080p_not_720p() {
        let v = info(1920, 800, None, None);
        assert_eq!(corrected_dims(&v), (1920, 800));
        assert_eq!(label_resolution(&v).as_deref(), Some("1080p"));
    }

    #[test]
    fn uhd_scope_3840x1600_is_2160p_not_1440p() {
        let v = info(3840, 1600, None, None);
        assert_eq!(label_resolution(&v).as_deref(), Some("2160p"));
    }

    #[test]
    fn dvd_dar_720x576_pixel_1024x576_display_is_576p() {
        let v = info(720, 576, Some(1024), Some(576));
        assert_eq!(corrected_dims(&v), (1024, 576));
        assert_eq!(label_resolution(&v).as_deref(), Some("576p"));
    }

    #[test]
    fn plain_1920x1080_is_1080p() {
        let v = info(1920, 1080, None, None);
        assert_eq!(label_resolution(&v).as_deref(), Some("1080p"));
    }

    #[test]
    fn tiny_and_missing_dims_are_unknown() {
        assert_eq!(label_resolution(&info(100, 100, None, None)), None);
        assert_eq!(label_resolution(&info(0, 1080, None, None)), None);
        assert_eq!(label_resolution(&info(1920, 0, None, None)), None);
    }

    #[test]
    fn remaining_bands() {
        assert_eq!(
            label_resolution(&info(7680, 4320, None, None)).as_deref(),
            Some("4320p")
        );
        assert_eq!(
            label_resolution(&info(2560, 1440, None, None)).as_deref(),
            Some("1440p")
        );
        assert_eq!(
            label_resolution(&info(1280, 720, None, None)).as_deref(),
            Some("720p")
        );
        assert_eq!(
            label_resolution(&info(720, 480, None, None)).as_deref(),
            Some("480p")
        );
    }
}
