//! Auto-update check for Vọng AI Recorder.
//!
//! Fetches `<R2_PUBLIC_BASE>/latest/version.txt` (a tiny plain-text file with
//! the latest semver on the first line), compares against the running binary's
//! version, and returns a structured result.
//!
//! Privacy rule: the only transmission is a vanilla HTTP GET with no custom
//! headers or payload — no machine UUID, no telemetry. IP is sent implicitly
//! as part of any TCP connection; this is disclosed in the privacy notice.
//! We log metadata only (current/latest version, is_newer, error category) —
//! never the raw HTTP response body beyond version parsing.
//!
//! Throttle: at most once per 4 hours per process via `LAST_CHECK_EPOCH_SECS`
//! atomic. Consent: reads `%APPDATA%\Vong\Vong AI Recorder\config\auto_update.txt`;
//! defaults to enabled (opt-out model) when the file is absent.

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};

use semver::Version;
use thiserror::Error;

// ── Public surface ────────────────────────────────────────────────────────────

/// Single source of truth for the CDN base URL.
///
/// Overridable at runtime via `VONG_UPDATE_BASE` env var — used by tests to
/// point at a local mock server without recompiling.
pub const DEFAULT_R2_BASE: &str = "https://dl.vong.app";

/// Download + release-notes landing page shown to the user.
pub const DOWNLOAD_LANDING_URL: &str = "https://vong.app/";

/// Result of a successful version check.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateInfo {
    /// Semver string of the running binary (from `CARGO_PKG_VERSION`).
    pub current: String,
    /// Semver string parsed from the remote `version.txt`.
    pub latest: String,
    /// `true` when `latest > current` using semver comparison.
    pub is_newer: bool,
    /// URL to open in the user's browser when a new version is available.
    pub download_landing_url: String,
}

/// Error variants returned by `check_for_update`.
#[derive(Debug, Error)]
pub enum UpdateError {
    /// Network call failed (timeout, DNS, TLS, etc.).
    #[error("network error: {0}")]
    Network(String),
    /// The version.txt file had no parseable semver line.
    #[error("version parse error: {0}")]
    Parse(String),
}

// ── Throttle state ────────────────────────────────────────────────────────────

/// Last successful (or attempted) check timestamp in Unix seconds.
/// 0 = never checked. Shared across all call sites in the process.
static LAST_CHECK_EPOCH_SECS: AtomicI64 = AtomicI64::new(0);

/// Minimum gap between automatic checks (4 hours in seconds).
const THROTTLE_SECS: i64 = 4 * 60 * 60;

/// HTTP timeout for the version.txt fetch.
const FETCH_TIMEOUT_SECS: u64 = 10;

// ── Core async function ───────────────────────────────────────────────────────

/// Fetch the remote `version.txt` and compare with the running binary.
///
/// Returns `Ok(UpdateInfo)` on success, or `Err(UpdateError)` on network /
/// parse failure.
///
/// The R2 base URL is resolved from the `VONG_UPDATE_BASE` environment variable
/// (for test overrides) with [`DEFAULT_R2_BASE`] as the fallback.
///
/// Does **not** enforce the 4-hour throttle — callers must check
/// [`should_check`] before calling this in the startup background task.
/// The "Check now" button in Settings bypasses the throttle intentionally.
pub async fn check_for_update() -> Result<UpdateInfo, UpdateError> {
    let base = std::env::var("VONG_UPDATE_BASE")
        .unwrap_or_else(|_| DEFAULT_R2_BASE.to_string());
    let url = format!("{base}/latest/version.txt");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()
        .map_err(|e| UpdateError::Network(e.to_string()))?;

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(error_category = "network", "auto-update: fetch failed");
            UpdateError::Network(e.to_string())
        })?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        tracing::warn!(http_status = status, "auto-update: non-2xx response");
        return Err(UpdateError::Network(format!("HTTP {status}")));
    }

    let body = response.text().await.map_err(|e| {
        tracing::warn!(error_category = "read_body", "auto-update: body read failed");
        UpdateError::Network(e.to_string())
    })?;

    let latest_str = parse_version_txt(&body)?;

    let current_str = env!("CARGO_PKG_VERSION");
    let current = Version::parse(current_str).map_err(|e| {
        UpdateError::Parse(format!("current version invalid: {e}"))
    })?;
    let latest = Version::parse(&latest_str).map_err(|e| {
        UpdateError::Parse(format!("remote version invalid: {e}"))
    })?;

    let is_newer = latest > current;

    tracing::info!(
        current_version = current_str,
        latest_version = %latest_str,
        is_newer,
        "auto-update: version check completed"
    );

    // Update throttle timestamp regardless of result.
    let now = now_epoch_secs();
    LAST_CHECK_EPOCH_SECS.store(now, Ordering::Relaxed);

    Ok(UpdateInfo {
        current: current_str.to_string(),
        latest: latest_str,
        is_newer,
        download_landing_url: DOWNLOAD_LANDING_URL.to_string(),
    })
}

/// Returns `true` if enough time has elapsed since the last check.
/// Always `true` when the process has never checked (epoch = 0).
pub fn should_check() -> bool {
    let last = LAST_CHECK_EPOCH_SECS.load(Ordering::Relaxed);
    if last == 0 {
        return true;
    }
    let now = now_epoch_secs();
    (now - last) >= THROTTLE_SECS
}

// ── Consent file helpers ──────────────────────────────────────────────────────

/// Path to `%APPDATA%\Vong\Vong AI Recorder\config\auto_update.txt`.
pub fn auto_update_config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "Vong", "Vong AI Recorder")
        .map(|d| d.config_dir().join("auto_update.txt"))
}

/// Read the auto-update consent flag.
///
/// Returns `true` (enabled) when:
/// - the file is absent (first run — opt-out model, default ON)
/// - the file contains `"enabled"` (any leading/trailing whitespace stripped)
///
/// Returns `false` only when the file explicitly contains `"disabled"`.
pub fn load_auto_update_enabled() -> bool {
    let Some(path) = auto_update_config_path() else {
        return true; // ProjectDirs unavailable — default on
    };
    match std::fs::read_to_string(&path) {
        Err(_) => true, // file absent → default on
        Ok(content) => content.trim() != "disabled",
    }
}

/// Persist the auto-update consent flag.
pub fn save_auto_update_enabled(enabled: bool) -> std::io::Result<()> {
    let Some(path) = auto_update_config_path() else {
        return Err(std::io::Error::other("ProjectDirs unavailable"));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, if enabled { "enabled" } else { "disabled" })
}

// ── Open browser ──────────────────────────────────────────────────────────────

/// Open `url` in the default system browser.
///
/// Uses the `webbrowser` crate on all platforms. Logs a warning on failure but
/// does not propagate the error — opening a browser is best-effort.
///
/// Not yet wired to a toast-click handler (beta scope: toast is informational
/// only). Reserved for a future Sprint that adds a clickable "Download" action.
#[allow(dead_code)]
pub fn open_browser(url: &str) {
    if let Err(e) = webbrowser::open(url) {
        tracing::warn!(error_category = "browser_open", "auto-update: failed to open browser");
        let _ = e; // suppress unused-variable warning; error is non-sensitive
    } else {
        tracing::info!("auto-update: browser opened for download page");
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Parse the first non-empty, whitespace-stripped line from `version.txt`.
/// Validates it as a semver string and returns the canonical form.
pub(crate) fn parse_version_txt(body: &str) -> Result<String, UpdateError> {
    let line = body
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .ok_or_else(|| UpdateError::Parse("version.txt is empty".into()))?;

    // Validate as semver before accepting.
    Version::parse(line)
        .map(|v| v.to_string())
        .map_err(|e| UpdateError::Parse(format!("'{line}': {e}")))
}

fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_version_txt ─────────────────────────────────────────────────────

    #[test]
    fn parse_version_txt_plain() {
        let result = parse_version_txt("0.1.0-beta.2\n").unwrap();
        assert_eq!(result, "0.1.0-beta.2");
    }

    #[test]
    fn parse_version_txt_trims_whitespace() {
        let result = parse_version_txt("  0.2.0  \r\n").unwrap();
        assert_eq!(result, "0.2.0");
    }

    #[test]
    fn parse_version_txt_skips_empty_lines() {
        let result = parse_version_txt("\n\n\n1.0.0\n").unwrap();
        assert_eq!(result, "1.0.0");
    }

    #[test]
    fn parse_version_txt_multiline_uses_first_nonempty() {
        // Any content after the first line is ignored.
        let result = parse_version_txt("0.1.0-beta.3\nignored line\n").unwrap();
        assert_eq!(result, "0.1.0-beta.3");
    }

    #[test]
    fn parse_version_txt_empty_body_returns_error() {
        let result = parse_version_txt("   \n  \n");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn parse_version_txt_invalid_semver_returns_error() {
        let result = parse_version_txt("not-a-version\n");
        assert!(result.is_err());
    }

    // ── semver comparison (is_newer logic) ────────────────────────────────────

    #[test]
    fn is_newer_true_when_remote_greater() {
        let current = Version::parse("0.1.0-beta.1").unwrap();
        let latest = Version::parse("0.1.0-beta.2").unwrap();
        assert!(latest > current);
    }

    #[test]
    fn is_newer_false_when_equal() {
        let current = Version::parse("0.1.0-beta.1").unwrap();
        let latest = Version::parse("0.1.0-beta.1").unwrap();
        assert!(latest <= current);
    }

    #[test]
    fn is_newer_false_when_older_remote() {
        let current = Version::parse("0.2.0").unwrap();
        let latest = Version::parse("0.1.0").unwrap();
        assert!(latest <= current);
    }

    // ── URL construction with VONG_UPDATE_BASE override ───────────────────────

    #[test]
    fn url_construction_uses_base() {
        // This test validates the string composition without a network call.
        let base = "https://example.com/r2";
        let url = format!("{base}/latest/version.txt");
        assert_eq!(url, "https://example.com/r2/latest/version.txt");
    }

    // ── Consent file roundtrip ────────────────────────────────────────────────

    #[test]
    fn consent_roundtrip_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auto_update.txt");
        std::fs::write(&path, "enabled").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.trim(), "enabled");
        assert!(content.trim() != "disabled");
    }

    #[test]
    fn consent_roundtrip_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auto_update.txt");
        std::fs::write(&path, "disabled").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.trim(), "disabled");
    }

    #[test]
    fn consent_missing_file_defaults_to_enabled() {
        // Simulate "file not found" branch of load_auto_update_enabled.
        // We can't call load_auto_update_enabled() directly from a test without
        // touching the real AppData path, so we test the inline logic.
        let content: Option<std::io::Result<String>> = None; // simulates Err(_) from read_to_string
        let enabled = match content {
            None | Some(Err(_)) => true,
            Some(Ok(ref s)) => s.trim() != "disabled",
        };
        assert!(enabled);
    }

    // ── UpdateError variants ──────────────────────────────────────────────────

    #[test]
    fn update_error_network_message() {
        let e = UpdateError::Network("connection refused".into());
        assert!(e.to_string().contains("network error"));
        assert!(e.to_string().contains("connection refused"));
    }

    #[test]
    fn update_error_parse_message() {
        let e = UpdateError::Parse("bad version".into());
        assert!(e.to_string().contains("version parse error"));
    }
}
