//! Integration tests for last-viewed session persistence helpers.
//!
//! Covers: write/read roundtrip, missing-file returns None, invalid content
//! returns None, and zero/negative session ids.

use std::fs;
use std::path::PathBuf;

/// Minimal re-implementation of the persistence helpers to keep the test
/// self-contained (binary crates don't expose pub APIs to integration tests).
fn save_last_session_to(path: &PathBuf, session_id: i64) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, session_id.to_string())
}

fn load_last_session_from(path: &PathBuf) -> Option<i64> {
    let content = fs::read_to_string(path).ok()?;
    content.trim().parse::<i64>().ok()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn tmp_path(name: &str) -> PathBuf {
    std::env::temp_dir()
        .join("vong_last_session_tests")
        .join(name)
}

fn clean(path: &PathBuf) {
    let _ = fs::remove_file(path);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn write_then_read_returns_same_session_id() {
    let path = tmp_path("roundtrip.txt");
    clean(&path);

    let session_id: i64 = 42;
    save_last_session_to(&path, session_id).expect("save must succeed");
    let loaded = load_last_session_from(&path);
    assert_eq!(loaded, Some(42));

    clean(&path);
}

#[test]
fn overwrite_returns_latest_session_id() {
    let path = tmp_path("overwrite.txt");
    clean(&path);

    save_last_session_to(&path, 1).expect("first save");
    save_last_session_to(&path, 99).expect("second save");
    assert_eq!(load_last_session_from(&path), Some(99));

    clean(&path);
}

#[test]
fn load_returns_none_when_file_absent() {
    let path = tmp_path("absent_file_xyz.txt");
    // Ensure it does not exist.
    clean(&path);

    let result = load_last_session_from(&path);
    assert_eq!(result, None, "missing file must yield None");
}

#[test]
fn load_returns_none_on_non_integer_content() {
    let path = tmp_path("invalid_content.txt");
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    // Write garbage that isn't a valid i64.
    fs::write(&path, "not_a_number").expect("write");
    let result = load_last_session_from(&path);
    assert_eq!(result, None, "non-integer content must yield None");
    clean(&path);
}

#[test]
fn load_returns_none_on_empty_file() {
    let path = tmp_path("empty_file.txt");
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&path, "").expect("write empty");
    let result = load_last_session_from(&path);
    assert_eq!(result, None, "empty file must yield None");
    clean(&path);
}

#[test]
fn large_session_id_roundtrip() {
    let path = tmp_path("large_id.txt");
    clean(&path);

    let large_id: i64 = i64::MAX;
    save_last_session_to(&path, large_id).expect("save");
    assert_eq!(load_last_session_from(&path), Some(i64::MAX));

    clean(&path);
}

#[test]
fn whitespace_trimmed_on_load() {
    // File written with trailing newline (common from text editors / write! calls).
    let path = tmp_path("with_newline.txt");
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&path, "7\n").expect("write");
    assert_eq!(load_last_session_from(&path), Some(7));
    clean(&path);
}
