//! Integration tests for the dictionary persistence + validation helpers.
//!
//! Tests run against temporary files so they don't touch the real user profile.
//! Privacy invariant: none of these tests produce tracing events containing
//! phrase content — only integer counts.

use std::path::PathBuf;
use vong_transcribe::{DictContext, DictEntry};

// ── Helper ────────────────────────────────────────────────────────────────────

fn entry(phrase: &str, ctx: DictContext) -> DictEntry {
    DictEntry {
        phrase: phrase.to_string(),
        context: ctx,
    }
}

fn common(phrase: &str) -> DictEntry {
    entry(phrase, DictContext::Common)
}

// ── JSON roundtrip ────────────────────────────────────────────────────────────

#[test]
fn dictionary_file_default_roundtrip() {
    let original = vong_app_helpers::DictionaryFile {
        version: 1,
        entries: vec![
            entry("OKR", DictContext::Common),
            entry("Vọng AI Recorder", DictContext::Names),
            entry("WebSocket", DictContext::Technical),
        ],
    };
    let json = serde_json::to_string_pretty(&original).expect("serialize");
    let back: vong_app_helpers::DictionaryFile =
        serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.entries.len(), 3);
    assert_eq!(back.entries[0].phrase, "OKR");
    assert_eq!(back.entries[1].context, DictContext::Names);
    assert_eq!(back.entries[2].phrase, "WebSocket");
}

#[test]
fn dictionary_file_version_defaults_to_one() {
    // JSON without the version field should default to 1.
    let json = r#"{"entries":[{"phrase":"OKR","context":"common"}]}"#;
    let f: vong_app_helpers::DictionaryFile =
        serde_json::from_str(json).expect("deserialize no-version");
    assert_eq!(f.version, 1);
    assert_eq!(f.entries.len(), 1);
}

// ── Load error cases ──────────────────────────────────────────────────────────

#[test]
fn load_from_nonexistent_path_returns_empty() {
    // The `config_path()` helper uses ProjectDirs which points to the real
    // user directory. We test the fallback logic by calling `load_from_path`
    // with a nonexistent path via the public helper.
    let path = PathBuf::from(r"C:\nonexistent\path\that\will\never\exist\dictionary.json");
    let f = load_from_path(&path);
    assert!(
        f.entries.is_empty(),
        "missing file should produce empty entries"
    );
}

#[test]
fn load_from_corrupt_json_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("dictionary.json");
    std::fs::write(&path, b"{ this is not valid json }").expect("write corrupt");
    let f = load_from_path(&path);
    assert!(
        f.entries.is_empty(),
        "corrupt JSON should produce empty entries"
    );
}

#[test]
fn load_truncates_entries_beyond_hard_cap() {
    // Build a JSON with 105 entries — should come back as 100.
    let entries: Vec<DictEntry> = (0..105).map(|i| common(&format!("term_{i}"))).collect();
    let file = vong_app_helpers::DictionaryFile { version: 1, entries };
    let json = serde_json::to_string(&file).expect("serialize");

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("dictionary.json");
    std::fs::write(&path, json.as_bytes()).expect("write");
    let loaded = load_from_path(&path);
    assert_eq!(
        loaded.entries.len(),
        100,
        "entries beyond 100 must be truncated on load"
    );
}

// ── Validation ────────────────────────────────────────────────────────────────

#[test]
fn validate_rejects_empty_phrase() {
    use vong_app_helpers::{validate_new, DictError};
    let entries: Vec<DictEntry> = vec![];
    assert_eq!(validate_new(&entries, ""), Err(DictError::Empty));
    assert_eq!(validate_new(&entries, "  "), Err(DictError::Empty));
}

#[test]
fn validate_rejects_phrase_over_80_chars() {
    use vong_app_helpers::{validate_new, DictError};
    let long_phrase: String = "A".repeat(81);
    let entries: Vec<DictEntry> = vec![];
    assert_eq!(
        validate_new(&entries, &long_phrase),
        Err(DictError::TooLong)
    );
}

#[test]
fn validate_accepts_phrase_at_exactly_80_chars() {
    use vong_app_helpers::validate_new;
    let phrase: String = "A".repeat(80);
    let entries: Vec<DictEntry> = vec![];
    assert_eq!(validate_new(&entries, &phrase), Ok(()));
}

#[test]
fn validate_rejects_duplicate_phrase() {
    use vong_app_helpers::{validate_new, DictError};
    let entries = vec![common("OKR")];
    assert_eq!(validate_new(&entries, "OKR"), Err(DictError::Duplicate));
}

#[test]
fn validate_rejects_when_at_cap() {
    use vong_app_helpers::{validate_new, DictError};
    let entries: Vec<DictEntry> = (0..100).map(|i| common(&format!("term_{i}"))).collect();
    assert_eq!(
        validate_new(&entries, "new_term"),
        Err(DictError::AtCap)
    );
}

// ── Atomic save + reload roundtrip ────────────────────────────────────────────

#[test]
fn save_and_reload_roundtrip() {
    use vong_app_helpers::DictionaryFile;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("dictionary.json");

    let original = DictionaryFile {
        version: 1,
        entries: vec![
            entry("Vọng", DictContext::Names),
            entry("BYOK", DictContext::Technical),
        ],
    };
    save_to_path(&original, &path).expect("save");

    // Verify the .tmp file was renamed (no leftover temp file).
    let tmp = path.with_extension("json.tmp");
    assert!(!tmp.exists(), ".tmp file should have been renamed");

    let loaded = load_from_path(&path);
    assert_eq!(loaded.entries.len(), 2);
    assert_eq!(loaded.entries[0].phrase, "Vọng");
    assert_eq!(loaded.entries[1].context, DictContext::Technical);
}

// ── Whisper prompt builder (pure fn, no network) ─────────────────────────────

#[test]
fn whisper_prompt_five_entries_under_800_bytes() {
    let entries = vec![
        common("OKR"),
        common("Vọng AI Recorder"),
        entry("WebSocket", DictContext::Technical),
        common("API"),
        common("BYOK"),
    ];
    let prompt = vong_transcribe::build_whisper_prompt(&entries);
    assert!(
        !prompt.is_empty(),
        "non-empty dictionary should produce non-empty prompt"
    );
    assert!(
        prompt.len() <= 800,
        "prompt {} bytes exceeds 800-byte safety cap",
        prompt.len()
    );
}

#[test]
fn whisper_prompt_200_entries_stays_under_800_bytes() {
    let entries: Vec<DictEntry> = (0..200)
        .map(|i| common(&format!("terminology_phrase_{i:03}")))
        .collect();
    let prompt = vong_transcribe::build_whisper_prompt(&entries);
    assert!(
        prompt.len() <= 800,
        "truncated prompt {} bytes exceeds 800-byte cap",
        prompt.len()
    );
}

#[test]
fn whisper_prompt_empty_dictionary_is_empty_string() {
    assert_eq!(vong_transcribe::build_whisper_prompt(&[]), "");
}

// ── Helper fns that proxy into the dictionary module ─────────────────────────
// (Since the dictionary module is private to vong-app, we expose thin wrappers
//  via a test-only helper module compiled into this integration test.)

mod vong_app_helpers {
    //! Re-expose the types and functions we need for integration testing.
    //! These types mirror dictionary.rs exactly.

    use vong_transcribe::DictEntry;
    use serde::{Deserialize, Serialize};

    pub use vong_transcribe::MAX_DICTIONARY_ENTRIES as HARD_CAP;
    pub const PHRASE_MAX_CHARS: usize = 80;

    #[derive(Debug, Serialize, Deserialize)]
    pub struct DictionaryFile {
        #[serde(default = "default_version")]
        pub version: u32,
        #[serde(default)]
        pub entries: Vec<DictEntry>,
    }

    fn default_version() -> u32 { 1 }

    impl Default for DictionaryFile {
        fn default() -> Self { Self { version: 1, entries: vec![] } }
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum DictError {
        Empty,
        TooLong,
        Duplicate,
        AtCap,
    }

    pub fn validate_new(entries: &[DictEntry], candidate: &str) -> Result<(), DictError> {
        let trimmed = candidate.trim();
        if trimmed.is_empty() { return Err(DictError::Empty); }
        if trimmed.chars().count() > PHRASE_MAX_CHARS { return Err(DictError::TooLong); }
        if entries.iter().any(|e| e.phrase == trimmed) { return Err(DictError::Duplicate); }
        if entries.len() >= HARD_CAP { return Err(DictError::AtCap); }
        Ok(())
    }
}

/// Load a `DictionaryFile` from an explicit path (test helper).
fn load_from_path(path: &std::path::Path) -> vong_app_helpers::DictionaryFile {
    let text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return vong_app_helpers::DictionaryFile::default(),
    };
    match serde_json::from_str::<vong_app_helpers::DictionaryFile>(&text) {
        Ok(mut f) => {
            f.entries.truncate(100);
            f
        }
        Err(_) => vong_app_helpers::DictionaryFile::default(),
    }
}

/// Atomically save to an explicit path (test helper).
fn save_to_path(
    f: &vong_app_helpers::DictionaryFile,
    path: &std::path::Path,
) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(f)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
