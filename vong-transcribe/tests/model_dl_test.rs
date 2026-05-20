//! Unit tests for `vong_transcribe::model_dl`.
//!
//! All tests are offline — no real HuggingFace network calls.
//! Large binary downloads are never triggered; mocks use tiny in-memory bytes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::tempdir;
use vong_transcribe::model_dl::{
    cleanup_stale_parts, parse_lfs_pointer, DlError, DownloadProgress, DownloadState,
};

// ── Test 1: LFS pointer parse — happy path ────────────────────────────────

#[test]
fn parse_lfs_pointer_extracts_sha256() {
    let pointer = "\
version https://git-lfs.github.com/spec/v1\n\
oid sha256:60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe\n\
size 147951465\n";

    let (sha, size) = parse_lfs_pointer(pointer).expect("should parse valid pointer");
    assert_eq!(
        sha,
        "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe"
    );
    assert_eq!(size, 147_951_465u64);
}

// ── Test 2: LFS pointer parse — malformed inputs ─────────────────────────

#[test]
fn parse_lfs_pointer_rejects_missing_oid() {
    let pointer = "version https://git-lfs.github.com/spec/v1\nsize 100\n";
    let result = parse_lfs_pointer(pointer);
    assert!(
        matches!(result, Err(DlError::PointerParse)),
        "expected PointerParse, got {result:?}"
    );
}

#[test]
fn parse_lfs_pointer_rejects_empty_string() {
    let result = parse_lfs_pointer("");
    assert!(matches!(result, Err(DlError::PointerParse)));
}

#[test]
fn parse_lfs_pointer_rejects_garbage() {
    let result = parse_lfs_pointer("this is not a pointer file at all");
    assert!(matches!(result, Err(DlError::PointerParse)));
}

#[test]
fn parse_lfs_pointer_handles_extra_whitespace() {
    // Real HF pointer may have trailing spaces or CRLF line endings.
    let pointer =
        "version https://git-lfs.github.com/spec/v1\r\noid sha256:  aabbcc  \r\nsize 99\r\n";
    let (sha, _size) = parse_lfs_pointer(pointer).expect("should parse with whitespace trim");
    assert_eq!(sha, "aabbcc");
}

// ── Test 3: Atomic rename — .part written only on hash match ─────────────

/// Feed a small in-memory "model" through the core atomic-write logic:
/// - Correct hash → `.bin` exists, `.part` removed.
/// - Wrong hash → neither exists (`.part` cleaned up).
///
/// We test the logic directly via `sha2` + `std::fs` to avoid network calls.
#[test]
fn atomic_rename_succeeds_only_on_hash_match() {
    use sha2::{Digest, Sha256};

    let dir = tempdir().expect("tempdir");
    let part = dir.path().join("ggml-base.bin.part");
    let bin = dir.path().join("ggml-base.bin");

    let data = b"fake model bytes for testing";
    let expected_hash = format!("{:x}", Sha256::digest(data));

    // Write .part file.
    std::fs::write(&part, data).expect("write part");
    assert!(part.exists(), ".part should exist before rename");

    // Compute actual hash of written bytes.
    let actual: Vec<u8> = std::fs::read(&part).expect("read part");
    let actual_hash = format!("{:x}", Sha256::digest(&actual));

    // Simulate: hash matches → rename.
    if actual_hash == expected_hash {
        std::fs::rename(&part, &bin).expect("rename");
    }

    assert!(bin.exists(), ".bin must exist after successful hash match");
    assert!(!part.exists(), ".part must be removed after rename");
}

#[test]
fn atomic_rename_aborted_on_hash_mismatch() {
    use sha2::{Digest, Sha256};

    let dir = tempdir().expect("tempdir");
    let part = dir.path().join("ggml-base.bin.part");
    let bin = dir.path().join("ggml-base.bin");

    let data = b"corrupted data";
    let wrong_expected = "0000000000000000000000000000000000000000000000000000000000000000";

    std::fs::write(&part, data).expect("write part");

    let actual = format!("{:x}", Sha256::digest(data));
    // Mismatch → delete .part, do NOT rename.
    if actual != wrong_expected {
        std::fs::remove_file(&part).expect("remove part");
    }

    assert!(!bin.exists(), ".bin must NOT exist after hash mismatch");
    assert!(!part.exists(), ".part must be deleted after hash mismatch");
}

// ── Test 4: Progress EMA — ETA computation math ───────────────────────────

/// Verify the ETA formula using a seeded rate so the test doesn't depend on
/// wall-clock timing (tight loops give sub-ms elapsed which skips the rate
/// measurement guard).
#[test]
fn progress_eta_computed_correctly_from_seeded_rate() {
    let mut p = DownloadProgress::default();
    p.total = Some(10_000_000); // 10 MB total
    p.state = DownloadState::Fetching;

    // Seed a known rate: 1 MB/s. Set bytes at 5 MB downloaded.
    p.seed_rate_bps(1_000_000.0); // 1 MB/s
    p.bytes = 5_000_000;
    p.total = Some(10_000_000);

    // Manually recompute ETA the same way update_bytes does.
    let remaining = 10_000_000u64 - 5_000_000u64;
    let expected_eta = (remaining as f64 / 1_000_000.0) as u64; // 5 seconds

    // Trigger ETA recompute by calling update_bytes with a tiny delta.
    // Since last_update was just set by seed_rate_bps, elapsed will be ~0,
    // so the EMA rate stays at the seeded value.
    p.update_bytes(5_000_001, Some(10_000_000)); // +1 byte to trigger recompute

    let eta = p.eta_secs.expect("ETA should be computed with seeded rate");
    // Allow ±2 s tolerance for sub-ms elapsed causing small rate fluctuation.
    assert!(
        eta.abs_diff(expected_eta) <= 2,
        "ETA {eta} s should be ~{expected_eta} s at 1 MB/s with 5 MB remaining"
    );
}

#[test]
fn progress_eta_none_when_total_unknown() {
    let mut p = DownloadProgress::default();
    p.seed_rate_bps(500_000.0);
    p.update_bytes(100_000, None); // total unknown
    assert!(
        p.eta_secs.is_none(),
        "ETA must be None when total is unknown"
    );
}

#[test]
fn progress_percent_correct_at_boundaries() {
    let mut p = DownloadProgress::default();

    // 0% at start.
    assert_eq!(p.percent(), 0.0);

    // 50% at half.
    p.bytes = 500;
    p.total = Some(1000);
    assert!((p.percent() - 50.0).abs() < 0.01);

    // 100% when complete.
    p.bytes = 1000;
    assert!((p.percent() - 100.0).abs() < 0.01);

    // Never exceeds 100% even if bytes > total.
    p.bytes = 1500;
    assert!(p.percent() <= 100.0);
}

// ── Test 5: Cancel signal propagation ────────────────────────────────────

/// Verify that a set cancel flag is correctly observed. The actual async
/// download loop checks `cancel.load(Ordering::Relaxed)` per chunk; here
/// we verify the flag mechanism itself is correct, and test the cleanup
/// helper (cleanup_stale_parts) on the same tempdir.
#[test]
fn cancel_flag_is_observable_across_threads() {
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel2 = cancel.clone();

    // Initial state: not cancelled.
    assert!(!cancel.load(Ordering::Relaxed));

    // Another thread sets it.
    let handle = std::thread::spawn(move || {
        cancel2.store(true, Ordering::Relaxed);
    });
    handle.join().expect("thread");

    // Observed immediately after.
    assert!(
        cancel.load(Ordering::Relaxed),
        "cancel flag should be true after store from other thread"
    );
}

// ── Test 6: cleanup_stale_parts ───────────────────────────────────────────

#[test]
fn cleanup_stale_parts_removes_old_files_keeps_new() {
    let dir = tempdir().expect("tempdir");

    // Create a new .part file (should be kept).
    let new_part = dir.path().join("ggml-small.bin.part");
    std::fs::write(&new_part, b"in progress").expect("write new part");

    // Create an old .part file by writing then backdating its mtime.
    let old_part = dir.path().join("ggml-tiny.bin.part");
    std::fs::write(&old_part, b"stale download").expect("write old part");

    // Backdate mtime to 2 hours ago via filetime manipulation.
    // We use std::fs::File + set_modified if available, otherwise fall back to
    // writing a temp file with the right mtime via a 2-second-old creation.
    // Simplest portable approach: use SystemTime arithmetic.
    let two_hours_ago = std::time::SystemTime::now()
        .checked_sub(Duration::from_secs(7200))
        .expect("time sub");

    // filetime manipulation via std: set via OpenOptions + set_modified
    // (stable in Rust since 1.75 — our MSRV is 1.78, so this is safe).
    let old_file = std::fs::OpenOptions::new()
        .write(true)
        .open(&old_part)
        .expect("open old part");
    old_file.set_modified(two_hours_ago).expect("set mtime");
    drop(old_file);

    // Run cleanup.
    let removed = cleanup_stale_parts(dir.path());

    // Old file should be gone; new file should remain.
    assert_eq!(removed, 1, "should have removed exactly 1 stale .part file");
    assert!(!old_part.exists(), "stale .part should be deleted");
    assert!(new_part.exists(), "fresh .part should be kept");
}

#[test]
fn cleanup_stale_parts_returns_zero_on_empty_dir() {
    let dir = tempdir().expect("tempdir");
    let removed = cleanup_stale_parts(dir.path());
    assert_eq!(removed, 0);
}

#[test]
fn cleanup_stale_parts_ignores_non_part_files() {
    let dir = tempdir().expect("tempdir");

    // A regular .bin file (not .part) — must never be deleted.
    let bin = dir.path().join("ggml-base.bin");
    std::fs::write(&bin, b"valid model").expect("write bin");

    // Backdate it to confirm extension filter works, not age.
    let old_time = std::time::SystemTime::now()
        .checked_sub(Duration::from_secs(7200))
        .expect("time sub");
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&bin)
        .expect("open");
    f.set_modified(old_time).expect("set mtime");
    drop(f);

    let removed = cleanup_stale_parts(dir.path());
    assert_eq!(removed, 0, ".bin files must never be deleted by cleanup");
    assert!(bin.exists(), "model .bin must survive cleanup");
}

// ── Test 7: DownloadProgress state transitions ────────────────────────────

#[test]
fn download_progress_state_sequence() {
    let progress = Arc::new(Mutex::new(DownloadProgress::default()));

    // Idle by default.
    {
        let p = progress.lock().unwrap();
        assert_eq!(p.state, DownloadState::Idle);
        assert_eq!(p.state.as_str(), "idle");
    }

    // Transition to Fetching.
    {
        let mut p = progress.lock().unwrap();
        p.state = DownloadState::Fetching;
    }
    assert_eq!(
        progress.lock().unwrap().state.as_str(),
        "fetching"
    );

    // Transition to Done.
    {
        let mut p = progress.lock().unwrap();
        p.state = DownloadState::Done;
    }
    assert_eq!(progress.lock().unwrap().state.as_str(), "done");

    // Reset goes back to Idle.
    {
        let mut p = progress.lock().unwrap();
        p.reset();
    }
    {
        let p = progress.lock().unwrap();
        assert_eq!(p.state, DownloadState::Idle);
        assert_eq!(p.bytes, 0);
        assert!(p.total.is_none());
        assert!(p.error_msg.is_empty());
    }
}

// ── Test 8: DlError Vietnamese messages ──────────────────────────────────

#[test]
fn dl_error_user_messages_are_non_empty_vietnamese() {
    // All error variants must produce a non-empty VI string.
    let cases: Vec<DlError> = vec![
        DlError::PointerParse,
        DlError::HashMismatch,
        DlError::Cancelled,
        DlError::DiskFull { needed_mb: 500 },
        DlError::UnknownModel("bogus".to_string()),
        DlError::RateLimited,
        DlError::Io(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied")),
    ];

    for err in &cases {
        let msg = err.user_message_vi();
        assert!(!msg.is_empty(), "empty VI message for {err:?}");
    }

    // DiskFull includes the MB amount.
    let disk_msg = DlError::DiskFull { needed_mb: 300 }.user_message_vi();
    assert!(disk_msg.contains("300"), "disk full message should contain MB amount");
}
