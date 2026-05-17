//! Vọng local storage subsystem.
//!
//! Phase 6: SQLite + FTS5 với `remove_diacritics=2` Vietnamese tone-insensitive search.
//! Schema per plan v2 Section 4.4 — sessions, transcript_segments, summaries.

#![warn(missing_docs)]
#![warn(clippy::all)]

/// Phase 0 placeholder. Phase 6 fills repository + search.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
