//! App version and GitHub Releases update check.
//!
//! The version is compiled in from `Cargo.toml` via `CARGO_PKG_VERSION`. CI bumps
//! `Cargo.toml` on release (see `.github/workflows/release.yml`) and builds from the
//! release tag, so released binaries always carry the right version with no manual step.

/// Version of the running binary, from `Cargo.toml` at compile time.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// GitHub repository the app checks for new releases (and links to from About).
pub const REPO_URL: &str = "https://github.com/maziluiosif/oxi";

/// The latest published GitHub release, as far as an update check is concerned.
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    /// Version from the release's `tag_name`, without the leading `v`.
    pub version: String,
    /// Web page of the release, for "View release".
    pub html_url: String,
}

/// Parse `X.Y.Z` (optionally prefixed with `v`; anything from a `-` or `+` on is
/// ignored) into a comparable tuple. CI only ever tags `vX.Y.Z`, so a full semver
/// implementation would be overkill here.
pub fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// True when `latest` is a strictly newer version than `current`. Unparseable versions
/// never count as newer (a malformed tag must not nag the user to "update").
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_semver(latest), parse_semver(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Fetch the latest release from the GitHub API. GitHub requires a `User-Agent` header;
/// unauthenticated calls are rate-limited to 60/hour per IP, which is plenty for a check
/// on startup plus a manual button.
pub async fn fetch_latest_release(client: &reqwest::Client) -> Result<ReleaseInfo, String> {
    let url = "https://api.github.com/repos/maziluiosif/oxi/releases/latest";
    let resp = client
        .get(url)
        .header("User-Agent", format!("oxi/{APP_VERSION}"))
        .header("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("GitHub API: HTTP {}", resp.status().as_u16()));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let tag = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or("GitHub API: response has no tag_name")?;
    let html_url = body
        .get("html_url")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{REPO_URL}/releases/latest"));
    Ok(ReleaseInfo {
        version: tag.trim_start_matches('v').to_string(),
        html_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_v_prefixed() {
        assert_eq!(parse_semver("0.8.0"), Some((0, 8, 0)));
        assert_eq!(parse_semver("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver(" v10.0.42 "), Some((10, 0, 42)));
    }

    #[test]
    fn ignores_prerelease_and_build_suffixes() {
        assert_eq!(parse_semver("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3+build5"), Some((1, 2, 3)));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_semver(""), None);
        assert_eq!(parse_semver("latest"), None);
        assert_eq!(parse_semver("1.2.3.4"), None);
        assert_eq!(parse_semver("1.x.0"), None);
    }

    #[test]
    fn newer_comparison() {
        assert!(is_newer("0.9.0", "0.8.0"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.8.0", "0.8.0"));
        assert!(!is_newer("0.7.9", "0.8.0"));
        // Malformed tags never count as an available update.
        assert!(!is_newer("latest", "0.8.0"));
    }

    #[test]
    fn app_version_is_parseable() {
        assert!(parse_semver(APP_VERSION).is_some());
    }
}
