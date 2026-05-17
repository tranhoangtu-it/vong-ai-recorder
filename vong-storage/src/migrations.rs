//! Schema migrations via `rusqlite_migration` with embedded SQL.

use rusqlite_migration::{Migrations, M};

/// All schema migrations, applied in order.
pub fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(include_str!("../sql/001_initial.sql"))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_validate() {
        let m = migrations();
        // rusqlite_migration validates SQL syntax + ordering on construction
        // (real validation happens via SQLite when applied — see db.rs tests).
        assert_eq!(m.current_version(&rusqlite::Connection::open_in_memory().unwrap()).unwrap(),
                   rusqlite_migration::SchemaVersion::NoneSet);
    }

    #[test]
    fn migrations_apply_clean() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        let m = migrations();
        m.to_latest(&mut conn).expect("migrations should apply cleanly");

        // Verify tables exist
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(tables.contains(&"sessions".to_string()));
        assert!(tables.contains(&"transcript_segments".to_string()));
        assert!(tables.contains(&"summaries".to_string()));
    }
}
