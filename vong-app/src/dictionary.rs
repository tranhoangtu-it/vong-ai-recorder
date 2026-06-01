//! Dictionary persistence — load/save/validate the user's custom vocabulary.
//!
//! Saved to `%APPDATA%\Vong\Vong AI Recorder\config\dictionary.json`.
//!
//! Format:
//! ```json
//! {
//!   "version": 1,
//!   "entries": [
//!     {"phrase": "Vọng AI Recorder", "context": "names"},
//!     {"phrase": "WebSocket",         "context": "technical"},
//!     {"phrase": "OKR",               "context": "common"}
//!   ]
//! }
//! ```
//!
//! PRIVACY RULE: Dictionary phrase strings are USER CONTENT.
//! NEVER pass them to `tracing::*` macros. Log only `entry_count` integers.

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use vong_transcribe::{DictContext, DictEntry, MAX_DICTIONARY_ENTRIES};


/// Maximum allowed number of dictionary entries. Matches the constant in
/// `vong-transcribe` — imported to keep a single source of truth.
pub const HARD_CAP: usize = MAX_DICTIONARY_ENTRIES;

/// Maximum character count per phrase (80 Unicode chars).
pub const PHRASE_MAX_CHARS: usize = 80;

// ────────────────────────────────────────────────────────────────────────────
// On-disk representation
// ────────────────────────────────────────────────────────────────────────────

/// Root structure for `dictionary.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct DictionaryFile {
    /// Schema version for forward-compat migration. Always written as 1.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Ordered list of user-defined vocabulary entries.
    #[serde(default)]
    pub entries: Vec<DictEntry>,
}

fn default_version() -> u32 {
    1
}

impl Default for DictionaryFile {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Path resolution
// ────────────────────────────────────────────────────────────────────────────

/// Resolve the path to `dictionary.json`.
/// Returns `None` if `ProjectDirs` is unavailable (shouldn't happen on Windows).
pub fn config_path() -> Option<PathBuf> {
    ProjectDirs::from("com", "Vong", "Vong AI Recorder")
        .map(|d| d.config_dir().join("dictionary.json"))
}

// ────────────────────────────────────────────────────────────────────────────
// Load / Save
// ────────────────────────────────────────────────────────────────────────────

/// Load `dictionary.json`.
///
/// Returns an empty `DictionaryFile` on any error (missing file, corrupt JSON,
/// etc.). Never panics. Entries exceeding `HARD_CAP` are silently truncated;
/// phrases over `PHRASE_MAX_CHARS` are truncated to that length.
pub fn load() -> DictionaryFile {
    let Some(path) = config_path() else {
        tracing::debug!("dictionary: ProjectDirs unavailable — using empty");
        return DictionaryFile::default();
    };

    let text = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {
            // First launch — no file yet; silently use empty.
            return DictionaryFile::default();
        }
        Err(e) => {
            tracing::warn!(error = %e, "dictionary.json read error — using empty");
            return DictionaryFile::default();
        }
    };

    match serde_json::from_str::<DictionaryFile>(&text) {
        Ok(mut f) => {
            // Enforce hard cap and phrase length limits.
            f.entries.truncate(HARD_CAP);
            for e in &mut f.entries {
                let char_count = e.phrase.chars().count();
                if char_count > PHRASE_MAX_CHARS {
                    // Truncate by char boundary (not byte boundary).
                    e.phrase = e.phrase.chars().take(PHRASE_MAX_CHARS).collect();
                }
            }
            // Privacy: log count only, never phrases.
            tracing::info!(entry_count = f.entries.len(), "dictionary loaded");
            f
        }
        Err(e) => {
            tracing::warn!(error = %e, "dictionary.json parse failed — using empty");
            DictionaryFile::default()
        }
    }
}

/// Atomically save `DictionaryFile` to disk.
///
/// Writes to a `.tmp` file first, then renames — ensures no torn writes.
/// Parent directory is created if it doesn't exist.
///
/// Privacy: only logs `entry_count`, never phrase content.
pub fn save(f: &DictionaryFile) -> std::io::Result<()> {
    let path = config_path()
        .ok_or_else(|| std::io::Error::other("ProjectDirs unavailable"))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(f)
        .map_err(|e| std::io::Error::other(format!("dictionary serialize: {e}")))?;
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, &path)?;
    // Privacy: log count only.
    tracing::info!(entry_count = f.entries.len(), "dictionary saved");
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// Validation
// ────────────────────────────────────────────────────────────────────────────

/// Error type for dictionary entry validation.
#[derive(Debug, PartialEq, Eq)]
pub enum DictError {
    /// Candidate phrase is empty after trimming.
    Empty,
    /// Phrase exceeds `PHRASE_MAX_CHARS` characters.
    TooLong,
    /// An identical phrase already exists in the dictionary.
    Duplicate,
    /// Dictionary is at the 100-entry hard cap.
    AtCap,
}

/// Validate a candidate phrase before adding it to the dictionary.
///
/// Returns `Ok(())` if the phrase is acceptable, `Err(DictError)` otherwise.
/// The caller must still push the entry and persist after validation passes.
pub fn validate_new(entries: &[DictEntry], candidate: &str) -> Result<(), DictError> {
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return Err(DictError::Empty);
    }
    if trimmed.chars().count() > PHRASE_MAX_CHARS {
        return Err(DictError::TooLong);
    }
    if entries.iter().any(|e| e.phrase == trimmed) {
        return Err(DictError::Duplicate);
    }
    if entries.len() >= HARD_CAP {
        return Err(DictError::AtCap);
    }
    Ok(())
}

/// Vietnamese-language user-visible error message for a `DictError`.
pub fn vi_error_msg(e: &DictError) -> &'static str {
    match e {
        DictError::Empty => "Mục không được rỗng",
        DictError::TooLong => "Mục không được dài quá 80 ký tự",
        DictError::Duplicate => "Mục đã tồn tại",
        DictError::AtCap => "Đã đạt giới hạn 100 mục — xoá mục cũ để thêm mới",
    }
}

/// Parse a context string from the UI (e.g. "names") into a `DictContext`.
/// Falls back to `DictContext::Common` for unrecognised values.
pub fn parse_context(s: &str) -> DictContext {
    match s.trim() {
        "names" => DictContext::Names,
        "technical" => DictContext::Technical,
        _ => DictContext::Common,
    }
}
