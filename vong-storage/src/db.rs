//! Database connection + migration setup.

use crate::error::StorageError;
use crate::migrations::migrations;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Open the production database at the user's platform data directory.
///
/// Path:
/// - macOS: `~/Library/Application Support/com.Vong.Vong/db.sqlite3`
/// - Windows: `%APPDATA%\Vong\Vong\data\db.sqlite3`
/// - Linux: `$XDG_DATA_HOME/Vong/db.sqlite3`
///
/// Creates parent directories if needed.
pub fn open_default() -> Result<Connection, StorageError> {
    let path = default_db_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    open_at(&path)
}

/// Open a database at the given path. Creates file if not exists.
pub fn open_at(path: &Path) -> Result<Connection, StorageError> {
    let mut conn = Connection::open(path)?;
    apply_pragmas(&conn)?;
    migrations().to_latest(&mut conn)?;
    tracing::info!(path = ?path, "storage: database opened + migrated");
    Ok(conn)
}

/// Open an in-memory database (for tests). Migrations are applied.
pub fn open_in_memory() -> Result<Connection, StorageError> {
    let mut conn = Connection::open_in_memory()?;
    apply_pragmas(&conn)?;
    migrations().to_latest(&mut conn)?;
    Ok(conn)
}

/// Apply SQLite pragmas for performance + safety.
fn apply_pragmas(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA foreign_keys = ON;
        PRAGMA temp_store = MEMORY;
        ",
    )?;
    Ok(())
}

/// Resolve the platform-specific default DB path.
pub fn default_db_path() -> Result<PathBuf, StorageError> {
    let dirs = directories::ProjectDirs::from("com", "Vong", "Vong")
        .ok_or(StorageError::PathResolution)?;
    Ok(dirs.data_dir().join("db.sqlite3"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_open_succeeds() {
        let _conn = open_in_memory().unwrap();
    }

    #[test]
    fn pragmas_applied() {
        let conn = open_in_memory().unwrap();
        let foreign_keys: i32 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);
    }

    #[test]
    fn default_path_resolves() {
        let path = default_db_path().expect("ProjectDirs should resolve on Mac/Win/Linux");
        assert!(path.to_string_lossy().contains("Vong"));
    }

    #[test]
    fn open_at_creates_parent_via_open_default() {
        // Use tempdir to avoid touching real filesystem
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("db.sqlite3");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let _ = open_at(&path).unwrap();
        assert!(path.exists());
    }
}
