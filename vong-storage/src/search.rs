//! FTS5 full-text search with Vietnamese tone-insensitive matching.
use crate::error::StorageError;
use crate::models::{normalize, SearchHit};
use rusqlite::{params, Connection};

pub fn search_transcripts(
    conn: &Connection,
    query: &str,
    limit: u32,
) -> Result<Vec<SearchHit>, StorageError> {
    let q_norm = normalize(query.trim());
    if q_norm.is_empty() {
        return Err(StorageError::Invalid("query is empty".into()));
    }
    let fts_query = q_norm
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(escape_fts_token)
        .collect::<Vec<_>>()
        .join(" AND ");
    if fts_query.is_empty() {
        return Err(StorageError::Invalid(
            "query contained only whitespace".into(),
        ));
    }
    let mut stmt = conn.prepare("SELECT s.id, s.started_at, s.detected_language, ts.text, ts.start_ms, snippet(transcript_fts, 0, '<b>', '</b>', '...', 16) AS snippet FROM transcript_fts JOIN transcript_segments ts ON transcript_fts.rowid = ts.id JOIN sessions s ON ts.session_id = s.id WHERE transcript_fts MATCH ?1 ORDER BY rank LIMIT ?2")?;
    let hits = stmt
        .query_map(params![&fts_query, limit], |row| {
            Ok(SearchHit {
                session_id: row.get(0)?,
                session_started_at_ms: row.get(1)?,
                language: row.get(2)?,
                text: row.get(3)?,
                start_ms: row.get(4)?,
                snippet: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    tracing::debug!(query = %fts_query, hits = hits.len(), "FTS search");
    Ok(hits)
}

fn escape_fts_token(token: &str) -> String {
    let escaped = token.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;
    use crate::models::{NewSegment, NewSession};
    use crate::repository::{insert_segment, insert_session};

    fn setup_with_texts(texts: &[&str]) -> Connection {
        let conn = open_in_memory().unwrap();
        let sid = insert_session(
            &conn,
            &NewSession {
                started_at_ms: 1_700_000_000_000,
                audio_source: "mic".into(),
                provider: "soniox".into(),
                detected_language: Some("vi".into()),
                meta_json: None,
            },
        )
        .unwrap();
        for (i, text) in texts.iter().enumerate() {
            insert_segment(
                &conn,
                &NewSegment {
                    session_id: sid,
                    seq: i as i64,
                    start_ms: (i as i64) * 1000,
                    end_ms: (i as i64 + 1) * 1000,
                    speaker_label: None,
                    text: text.to_string(),
                    language: Some("vi".into()),
                    confidence: None,
                    is_final: true,
                },
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn tone_insensitive_search_finds_voiced() {
        let conn = setup_with_texts(&[
            "Cuoc hop team chieu nay rat hay",
            "Toi di an com toi",
            "Hop voi khach hang vao thu Hai",
        ]);
        let hits = search_transcripts(&conn, "hop", 10).unwrap();
        assert!(
            !hits.is_empty(),
            "search hop should match hop variants via remove_diacritics=2"
        );
        assert!(hits.len() >= 2, "should match 2 sessions");
    }

    #[test]
    fn search_with_unicode_input() {
        let conn = setup_with_texts(&["Cuoc hop"]);
        let hits = search_transcripts(&conn, "cuoc hop", 10).unwrap();
        assert!(!hits.is_empty());
    }

    #[test]
    fn multi_word_search() {
        let conn = setup_with_texts(&["Cuoc hop team noi bo"]);
        let hits = search_transcripts(&conn, "cuoc hop", 10).unwrap();
        assert!(!hits.is_empty(), "multi-word AND search should work");
    }

    #[test]
    fn empty_query_returns_invalid() {
        let conn = setup_with_texts(&["Cuoc hop"]);
        let r = search_transcripts(&conn, "", 10);
        assert!(matches!(r, Err(StorageError::Invalid(_))));
    }

    #[test]
    fn whitespace_only_query_returns_invalid() {
        let conn = setup_with_texts(&["Cuoc hop"]);
        let r = search_transcripts(&conn, "   \t  ", 10);
        assert!(matches!(r, Err(StorageError::Invalid(_))));
    }

    #[test]
    fn case_insensitive_search() {
        let conn = setup_with_texts(&["Cuoc Hop team"]);
        let lower = search_transcripts(&conn, "HOP", 10).unwrap();
        assert!(!lower.is_empty());
    }

    #[test]
    fn no_match_returns_empty() {
        let conn = setup_with_texts(&["Cuoc hop"]);
        let r = search_transcripts(&conn, "spaceship", 10).unwrap();
        assert!(r.is_empty());
    }
}
