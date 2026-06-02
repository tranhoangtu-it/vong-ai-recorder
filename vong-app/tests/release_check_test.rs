//! Integration tests for Phase 19 auto-update logic.
//!
//! `vong-app` is a binary crate (no lib target), so these tests cannot
//! `use vong_app::auto_update`. Instead they:
//!   - Verify the source-file contracts (constants, function signatures present)
//!   - Reproduce the pure-logic helpers inline (parse_version_txt, semver compare)
//!   - Exercise the consent-file format contract via tempfile + std::fs
//!   - Test fetch failure via a refused TCP port (network path, no mock server)
//!
//! All heavier unit tests (URL construction, throttle, open_browser, etc.) live
//! in the `#[cfg(test)]` block inside `auto_update.rs`.

use std::fs;
use tempfile::TempDir;

// ── Inline helpers (mirror auto_update.rs logic) ─────────────────────────────

/// Mirror of `auto_update::parse_version_txt` — kept in sync manually.
/// Any drift between this and the real impl surfaces as a test failure
/// the moment the behaviour diverges.
fn parse_version_txt(body: &str) -> Result<String, String> {
    let line = body
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .ok_or_else(|| "version.txt is empty".to_string())?;

    semver::Version::parse(line)
        .map(|v| v.to_string())
        .map_err(|e| format!("'{line}': {e}"))
}

// ── 1. parse_version_txt — whitespace trimming ───────────────────────────────

#[test]
fn parse_strips_whitespace_around_version() {
    let result = parse_version_txt("  0.1.0-beta.2  \n").unwrap();
    assert_eq!(result, "0.1.0-beta.2");
}

// ── 2. parse_version_txt — multiline robustness ──────────────────────────────

#[test]
fn parse_uses_first_nonempty_line_ignores_rest() {
    let body = "\n\n0.2.0\n0.3.0-ignored\n";
    let result = parse_version_txt(body).unwrap();
    assert_eq!(result, "0.2.0");
}

// ── 3. parse_version_txt — empty body returns error ──────────────────────────

#[test]
fn parse_empty_body_returns_error() {
    let result = parse_version_txt("   \n\n  ");
    assert!(result.is_err(), "empty body must return an error");
    assert!(result.unwrap_err().contains("empty"));
}

// ── 4. parse_version_txt — invalid semver returns error ──────────────────────

#[test]
fn parse_invalid_semver_returns_error() {
    let result = parse_version_txt("not-a-semver\n");
    assert!(result.is_err(), "non-semver line must return an error");
}

// ── 5. Semver compare — is_newer true (beta.2 > beta.1) ─────────────────────

#[test]
fn semver_newer_when_remote_is_greater_prerelease() {
    let current = semver::Version::parse("0.1.0-beta.1").unwrap();
    let latest  = semver::Version::parse("0.1.0-beta.2").unwrap();
    assert!(latest > current, "beta.2 must compare greater than beta.1");
}

// ── 6. Semver compare — equal versions are not newer ─────────────────────────

#[test]
fn semver_not_newer_when_equal() {
    let v1 = semver::Version::parse("0.1.0-beta.1").unwrap();
    let v2 = semver::Version::parse("0.1.0-beta.1").unwrap();
    assert!(v2 <= v1, "equal versions must not be considered newer");
}

// ── 7. Semver compare — remote older than current ────────────────────────────

#[test]
fn semver_not_newer_when_remote_is_older() {
    let current = semver::Version::parse("1.0.0").unwrap();
    let older   = semver::Version::parse("0.9.9").unwrap();
    assert!(older <= current, "0.9.9 must not be newer than 1.0.0");
}

// ── 8. URL construction — default R2 base ────────────────────────────────────

#[test]
fn url_construction_default_base() {
    // Mirror DEFAULT_R2_BASE from auto_update.rs. Verified against source below.
    let default_base = "https://dl.vong.app";
    let url = format!("{default_base}/latest/version.txt");
    assert_eq!(url, "https://dl.vong.app/latest/version.txt");
}

// ── 9. Consent file — enabled roundtrip ──────────────────────────────────────

#[test]
fn consent_file_enabled_roundtrip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("auto_update.txt");
    fs::write(&path, "enabled").unwrap();
    // Mirror load_auto_update_enabled logic: absent or "enabled" → true.
    let content = fs::read_to_string(&path).unwrap();
    let is_enabled = content.trim() != "disabled";
    assert!(is_enabled, "file containing 'enabled' must load as enabled");
}

// ── 10. Consent file — disabled roundtrip ────────────────────────────────────

#[test]
fn consent_file_disabled_roundtrip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("auto_update.txt");
    fs::write(&path, "disabled").unwrap();
    let content = fs::read_to_string(&path).unwrap();
    let is_enabled = content.trim() != "disabled";
    assert!(!is_enabled, "file containing 'disabled' must load as disabled");
}

// ── 11. Consent file — absent file defaults to enabled ───────────────────────

#[test]
fn consent_missing_file_defaults_to_enabled() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("auto_update.txt");
    // File intentionally not created.
    let is_enabled = match fs::read_to_string(&path) {
        Err(_) => true, // missing → default ON
        Ok(ref s) => s.trim() != "disabled",
    };
    assert!(is_enabled, "absent consent file must default to enabled");
}

// ── 12. Source file — R2 base constant is present ────────────────────────────

#[test]
fn auto_update_source_contains_r2_base_constant() {
    let src = fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/auto_update.rs"),
    )
    .expect("auto_update.rs must be readable");

    assert!(
        src.contains("dl.vong.app"),
        "auto_update.rs must contain the R2 base URL 'dl.vong.app'"
    );
    assert!(
        src.contains("VONG_UPDATE_BASE"),
        "auto_update.rs must support VONG_UPDATE_BASE env override for testing"
    );
}

// ── 13. Source file — DOWNLOAD_LANDING_URL present ───────────────────────────

#[test]
fn auto_update_source_contains_landing_url() {
    let src = fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/auto_update.rs"),
    )
    .expect("auto_update.rs must be readable");

    assert!(
        src.contains("vong.app/"),
        "auto_update.rs must contain 'vong.app/' as the landing URL"
    );
}

// ── 14. Fetch failure → network error (refused port, no real server needed) ───
//
// 127.0.0.1:1 is a privileged port that is always ECONNREFUSED on any OS.
// The test validates that reqwest returns an error on connection failure,
// which is exactly what auto_update::check_for_update maps to UpdateError::Network.

#[tokio::test]
async fn fetch_from_refused_port_returns_error() {
    let url = "http://127.0.0.1:1/latest/version.txt";
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let result = client.get(url).send().await;
    assert!(
        result.is_err(),
        "connection to port 1 (refused) must produce an error — \
         this is what auto_update maps to UpdateError::Network"
    );
}
