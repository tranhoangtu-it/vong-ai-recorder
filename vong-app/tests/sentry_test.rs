//! Phase 9 — Sentry crash reporting: compile-level integration checks.
//!
//! `vong-app` is a binary crate (no lib target), so this integration test
//! cannot `use vong_app::…`. Instead it verifies:
//!
//! 1. The `SENTRY_DSN` placeholder is present in the source so a CI grep step
//!    can catch accidental real DSN commits, AND so `init_if_enabled` stays
//!    safely inert until the user pastes in the real DSN pre-release.
//!
//! 2. The consent file format contract is exercised via `tempfile` + direct
//!    std::fs I/O (same logic as the in-module unit tests, kept here as a
//!    second opinion at integration level without needing to import the crate).
//!
//! All heavy `before_send` + state-machine tests live in the in-module
//! `#[cfg(test)]` blocks of `sentry_init.rs` and `wizard.rs`.

use std::fs;
use tempfile::TempDir;

// ── Consent file format contract ─────────────────────────────────────────────
// These mirror the in-module tests but run as integration tests (separate
// process). They verify the format contract without importing the crate.

fn write_consent(dir: &TempDir, value: &str) -> std::path::PathBuf {
    let path = dir.path().join("sentry.txt");
    fs::write(&path, value).expect("write sentry.txt");
    path
}

fn read_consent_matches(path: &std::path::Path, expected: &str) -> bool {
    fs::read_to_string(path)
        .map(|s| s.trim() == expected)
        .unwrap_or(false)
}

#[test]
fn consent_file_format_enabled_value() {
    let dir = TempDir::new().expect("tempdir");
    let path = write_consent(&dir, "enabled\n");
    assert!(
        read_consent_matches(&path, "enabled"),
        "enabled consent file must trim to the string 'enabled'"
    );
}

#[test]
fn consent_file_format_disabled_value() {
    let dir = TempDir::new().expect("tempdir");
    let path = write_consent(&dir, "disabled\n");
    assert!(
        read_consent_matches(&path, "disabled"),
        "disabled consent file must trim to the string 'disabled'"
    );
}

#[test]
fn consent_file_garbage_does_not_match_known_values() {
    let dir = TempDir::new().expect("tempdir");
    let path = write_consent(&dir, "yes_please\n");
    assert!(
        !read_consent_matches(&path, "enabled"),
        "garbage value must not match 'enabled'"
    );
    assert!(
        !read_consent_matches(&path, "disabled"),
        "garbage value must not match 'disabled'"
    );
}

// ── DSN placeholder — source-file level check ────────────────────────────────

#[test]
fn sentry_init_source_contains_replace_me_placeholder() {
    // Read the actual source file to confirm the placeholder is present.
    // This catches cases where a developer pastes in a real DSN without
    // following the documented pre-merge procedure.
    //
    // CI can duplicate this as a simpler `grep REPLACE-ME src/sentry_init.rs`
    // step, but having it as a test means `cargo test` also catches it.
    let src = fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/sentry_init.rs"),
    )
    .expect("sentry_init.rs must be readable");

    assert!(
        src.contains("REPLACE-ME"),
        "sentry_init.rs must still contain the REPLACE-ME placeholder DSN. \
         If you have a real Vong-owned Sentry DSN, paste it into SENTRY_DSN \
         following the pre-merge procedure in the module doc, then delete \
         or update this test."
    );
}

// ── Wizard step count sanity (compile-time values in source) ─────────────────

#[test]
fn wizard_source_contains_crash_report_variant() {
    let src = fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/wizard.rs"),
    )
    .expect("wizard.rs must be readable");

    assert!(
        src.contains("CrashReport"),
        "wizard.rs must contain the CrashReport enum variant (Phase 9)"
    );
    assert!(
        src.contains("crash_report"),
        "wizard.rs must contain the 'crash_report' step key for persistence"
    );
}

#[test]
fn wizard_source_done_step_is_index_seven() {
    let src = fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/wizard.rs"),
    )
    .expect("wizard.rs must be readable");

    // Check that Done maps to 7, not 6, after Phase 9 insertion.
    assert!(
        src.contains("Self::Done => 7"),
        "wizard.rs: Done step must be at index 7 after Phase 9 CrashReport insertion"
    );
}
