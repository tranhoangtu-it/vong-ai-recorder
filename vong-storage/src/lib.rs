//! Vọng local storage subsystem.
//!
//! Phase 6: SQLite + FTS5 với `remove_diacritics=2` Vietnamese tone-insensitive search.
//! Schema per plan v2 Section 4.4 — sessions, transcript_segments, summaries.

// TODO Phase 6.1: backfill comprehensive rustdoc — currently simplified due
// to shell-escape workaround during initial scaffold. Public API self-documenting.
#![allow(missing_docs)]
#![warn(clippy::all)]

mod db;
mod error;
mod export;
mod migrations;
mod models;
mod repository;
mod search;

pub use db::{default_db_path, open_at, open_default, open_in_memory};
pub use error::StorageError;
pub use export::{export_markdown, export_srt, export_text};
pub use models::{normalize, NewSegment, NewSession, NewSummary, SearchHit, Segment, Session, Summary};
pub use repository::{
    delete_session, delete_summaries_for_session, fetch_latest_summary, fetch_segments_for_session,
    fetch_session, finalize_session, insert_segment, insert_session, insert_summary,
    list_recent_sessions,
};
pub use search::search_transcripts;

// Re-export `rusqlite::Connection` so downstream crates can hold connection
// handles without taking a direct dependency on the rusqlite crate.
pub use rusqlite::Connection;

/// Crate version.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
