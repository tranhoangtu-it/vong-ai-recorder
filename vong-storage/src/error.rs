//! Storage error types.

use thiserror::Error;

/// Errors emitted by the storage subsystem.
#[derive(Debug, Error)]
pub enum StorageError {
    /// SQLite-level error.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Migration failed.
    #[error("migration failed: {0}")]
    Migration(String),

    /// Filesystem error (path resolution, IO).
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),

    /// Could not determine platform-specific data directory.
    #[error("could not determine project data directory")]
    PathResolution,

    /// Session not found by ID.
    #[error("session not found: id={0}")]
    SessionNotFound(i64),

    /// Invalid input parameter.
    #[error("invalid input: {0}")]
    Invalid(String),
}

impl From<rusqlite_migration::Error> for StorageError {
    fn from(e: rusqlite_migration::Error) -> Self {
        Self::Migration(e.to_string())
    }
}
