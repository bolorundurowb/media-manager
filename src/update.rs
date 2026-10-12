//! Checks GitHub for a newer release of media-manager.
//!
//! Compiled only with the `gui` feature (it needs an HTTP client). The pure
//! pieces — version parsing/comparison and release-payload parsing — are
//! separate from the network call so they can be unit tested offline.

use std::time::Duration;

/// Version of the running build.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/bolorundurowb/media-manager/releases/latest";
/// Fallback target for the call to action when the API gives no usable URL.
pub const RELEASES_URL: &str = "https://github.com/bolorundurowb/media-manager/releases/latest";

/// Set to any value to disable background update checks.
pub const OPT_OUT_ENV: &str = "MEDIA_MANAGER_NO_UPDATE_CHECK";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A release newer than the running version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    /// Version without the leading `v`, e.g. `0.4.0`.
    pub version: String,
    /// Page of the release on GitHub, where the assets can be downloaded.
    pub url: String,
}

/// Parses `v1.2.3`, `1.2` or `1.2.3-rc.1` into `(major, minor, patch)`.
/// Pre-release and build suffixes are ignored; missing parts count as 0.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let text = text.trim();
    let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
    let core = text.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// True when `latest` is strictly newer than `current`. Unparseable input is
/// never "newer", so a malformed tag cannot trigger a notification.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

/// Interprets the body of GitHub's "latest release" endpoint.
pub fn parse_release(body: &str, current: &str) -> Result<Option<UpdateInfo>, String> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid release response: {e}"))?;
    let tag = json
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or("release response has no tag_name")?;
    if !is_newer(tag, current) {
        return Ok(None);
    }
    // Only ever send the user to github.com, whatever the payload says.
    let url = json
        .get("html_url")
        .and_then(|v| v.as_str())
        .filter(|u| u.starts_with("https://github.com/"))
        .unwrap_or(RELEASES_URL);
    Ok(Some(UpdateInfo {
        version: tag.trim().trim_start_matches(['v', 'V']).to_string(),
        url: url.to_string(),
    }))
}

/// Asks GitHub for the latest release and compares it with `current`.
pub fn check_latest(current: &str) -> Result<Option<UpdateInfo>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .into();
    let mut response = agent
        .get(LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        // GitHub rejects API requests that carry no User-Agent.
        .header("User-Agent", format!("media-manager/{current}"))
        .call()
        .map_err(|e| e.to_string())?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    parse_release(&body, current)
}

/// Whether the user opted out via [`OPT_OUT_ENV`].
pub fn disabled_by_env() -> bool {
    std::env::var_os(OPT_OUT_ENV).is_some()
}

/// Checks now and then every `interval` on a background thread. `notify` is
/// called with each newer release found and returns `false` to stop the
/// thread (e.g. when the receiving side is gone). Network failures are
/// logged at debug level and retried on the next tick, so being offline is
/// silent.
pub fn spawn_periodic(
    current: &'static str,
    interval: Duration,
    mut notify: impl FnMut(UpdateInfo) -> bool + Send + 'static,
) {
    let spawned = std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || loop {
            match check_latest(current) {
                Ok(Some(info)) => {
                    if !notify(info) {
                        return;
                    }
                }
                Ok(None) => {}
                Err(err) => tracing::debug!(error = %err, "update check failed"),
            }
            std::thread::sleep(interval);
        });
    if let Err(err) = spawned {
        tracing::debug!(error = %err, "could not start update checker");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_version_shapes() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version(" V0.4 "), Some((0, 4, 0)));
        assert_eq!(parse_version("2"), Some((2, 0, 0)));
        assert_eq!(parse_version("1.2.3-rc.1+build.5"), Some((1, 2, 3)));
    }

    #[test]
    fn rejects_malformed_versions() {
        for bad in ["", "v", "latest", "1.x.3", "1.2.3.4", "-1.0.0"] {
            assert_eq!(parse_version(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn compares_numerically_not_lexically() {
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(is_newer("0.3.1", "0.3.0"));
        assert!(!is_newer("0.3.0", "0.3.0"));
        assert!(!is_newer("v0.2.9", "0.3.0"));
        assert!(!is_newer("garbage", "0.3.0"));
        assert!(!is_newer("0.4.0", "garbage"));
    }

    #[test]
    fn newer_release_yields_info() {
        let body = r#"{"tag_name":"v0.4.0","html_url":"https://github.com/bolorundurowb/media-manager/releases/tag/v0.4.0"}"#;
        let info = parse_release(body, "0.3.0").unwrap().unwrap();
        assert_eq!(info.version, "0.4.0");
        assert_eq!(
            info.url,
            "https://github.com/bolorundurowb/media-manager/releases/tag/v0.4.0"
        );
    }

    #[test]
    fn same_or_older_release_yields_nothing() {
        let body =
            r#"{"tag_name":"v0.3.0","html_url":"https://github.com/x/y/releases/tag/v0.3.0"}"#;
        assert_eq!(parse_release(body, "0.3.0").unwrap(), None);
        assert_eq!(parse_release(body, "0.3.1").unwrap(), None);
    }

    #[test]
    fn foreign_or_missing_url_falls_back_to_releases_page() {
        let foreign = r#"{"tag_name":"v9.0.0","html_url":"https://evil.example/download"}"#;
        assert_eq!(
            parse_release(foreign, "0.3.0").unwrap().unwrap().url,
            RELEASES_URL
        );
        let missing = r#"{"tag_name":"v9.0.0"}"#;
        assert_eq!(
            parse_release(missing, "0.3.0").unwrap().unwrap().url,
            RELEASES_URL
        );
    }

    #[test]
    fn bad_payloads_are_errors() {
        assert!(parse_release("not json", "0.3.0").is_err());
        assert!(parse_release(r#"{"name":"x"}"#, "0.3.0").is_err());
    }

    #[test]
    fn current_version_is_parseable() {
        assert!(parse_version(CURRENT_VERSION).is_some());
    }
}
