//! CRUD operations.
use crate::error::StorageError;
use crate::models::{normalize, NewSegment, NewSession, NewSummary, Segment, Session, Summary};
use rusqlite::{params, Connection};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn insert_session(conn: &Connection, s: &NewSession) -> Result<i64, StorageError> {
    conn.execute(
        "INSERT INTO sessions (started_at, audio_source, provider, detected_language, meta_json) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![s.started_at_ms, s.audio_source, s.provider, s.detected_language, s.meta_json],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn insert_segment(conn: &Connection, seg: &NewSegment) -> Result<i64, StorageError> {
    let text_norm = normalize(&seg.text);
    conn.execute(
        "INSERT INTO transcript_segments (session_id, seq, start_ms, end_ms, speaker_label, text, language, confidence, is_final) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![seg.session_id, seg.seq, seg.start_ms, seg.end_ms, seg.speaker_label, text_norm, seg.language, seg.confidence, seg.is_final as i64],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn finalize_session(conn: &Connection, session_id: i64) -> Result<(), StorageError> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let updated = conn.execute(
        "UPDATE sessions SET ended_at = ?1, duration_ms = ?1 - started_at WHERE id = ?2",
        params![now_ms, session_id],
    )?;
    if updated == 0 {
        return Err(StorageError::SessionNotFound(session_id));
    }
    Ok(())
}

pub fn fetch_session(conn: &Connection, id: i64) -> Result<Session, StorageError> {
    conn.query_row(
        "SELECT id, started_at, ended_at, duration_ms, audio_source, provider, detected_language, audio_path, meta_json FROM sessions WHERE id = ?1",
        [id],
        |row| Ok(Session {
            id: row.get(0)?, started_at_ms: row.get(1)?, ended_at_ms: row.get(2)?, duration_ms: row.get(3)?,
            audio_source: row.get(4)?, provider: row.get(5)?, detected_language: row.get(6)?,
            audio_path: row.get(7)?, meta_json: row.get(8)?,
        }),
    ).map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => StorageError::SessionNotFound(id),
        other => StorageError::Sqlite(other),
    })
}

pub fn fetch_segments_for_session(
    conn: &Connection,
    session_id: i64,
) -> Result<Vec<Segment>, StorageError> {
    let mut stmt = conn.prepare("SELECT id, session_id, seq, start_ms, end_ms, speaker_label, text, language, confidence, is_final FROM transcript_segments WHERE session_id = ?1 ORDER BY seq ASC")?;
    let rows = stmt
        .query_map([session_id], |row| {
            let is_final_int: i64 = row.get(9)?;
            Ok(Segment {
                id: row.get(0)?,
                session_id: row.get(1)?,
                seq: row.get(2)?,
                start_ms: row.get(3)?,
                end_ms: row.get(4)?,
                speaker_label: row.get(5)?,
                text: row.get(6)?,
                language: row.get(7)?,
                confidence: row.get(8)?,
                is_final: is_final_int != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_recent_sessions(conn: &Connection, limit: u32) -> Result<Vec<Session>, StorageError> {
    let mut stmt = conn.prepare("SELECT id, started_at, ended_at, duration_ms, audio_source, provider, detected_language, audio_path, meta_json FROM sessions ORDER BY started_at DESC LIMIT ?1")?;
    let rows = stmt
        .query_map([limit], |row| {
            Ok(Session {
                id: row.get(0)?,
                started_at_ms: row.get(1)?,
                ended_at_ms: row.get(2)?,
                duration_ms: row.get(3)?,
                audio_source: row.get(4)?,
                provider: row.get(5)?,
                detected_language: row.get(6)?,
                audio_path: row.get(7)?,
                meta_json: row.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn delete_session(conn: &Connection, id: i64) -> Result<(), StorageError> {
    let n = conn.execute("DELETE FROM sessions WHERE id = ?1", [id])?;
    if n == 0 {
        return Err(StorageError::SessionNotFound(id));
    }
    Ok(())
}

/// Insert a new summary row. Returns the row id.
pub fn insert_summary(conn: &Connection, s: &NewSummary) -> Result<i64, StorageError> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO summaries (session_id, kind, llm_provider, llm_model, content, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![s.session_id, s.kind, s.llm_provider, s.llm_model, s.content, now_ms],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Fetch the most-recent summary row for a session, if any.
pub fn fetch_latest_summary(
    conn: &Connection,
    session_id: i64,
) -> Result<Option<Summary>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, kind, llm_provider, llm_model, content, created_at
         FROM summaries WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
    )?;
    let result = stmt.query_row([session_id], |row| {
        Ok(Summary {
            id: row.get(0)?,
            session_id: row.get(1)?,
            kind: row.get(2)?,
            llm_provider: row.get(3)?,
            llm_model: row.get(4)?,
            content: row.get(5)?,
            created_at_ms: row.get(6)?,
        })
    });
    match result {
        Ok(s) => Ok(Some(s)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(StorageError::Sqlite(e)),
    }
}

/// Fetch the most-recent summary for each of the given session ids in one query.
/// Returns a map of session_id → Summary. Sessions with no summary are absent from the map.
/// Uses a single SQL query to avoid N+1 patterns when rendering the HistoryCard.
pub fn fetch_summaries_for_sessions(
    conn: &Connection,
    session_ids: &[i64],
) -> Result<std::collections::HashMap<i64, Summary>, StorageError> {
    if session_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    // Strategy: fetch MAX(id) per session_id in the given set, then join back.
    // This is a single pass with N bind params (one per session id).
    let placeholders = (1..=session_ids.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT s.id, s.session_id, s.kind, s.llm_provider, s.llm_model, s.content, s.created_at
         FROM summaries s
         INNER JOIN (
             SELECT MAX(id) AS max_id
             FROM summaries
             WHERE session_id IN ({placeholders})
             GROUP BY session_id
         ) latest ON s.id = latest.max_id"
    );
    let params: Vec<rusqlite::types::Value> = session_ids
        .iter()
        .map(|&id| rusqlite::types::Value::Integer(id))
        .collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok(Summary {
                id: row.get(0)?,
                session_id: row.get(1)?,
                kind: row.get(2)?,
                llm_provider: row.get(3)?,
                llm_model: row.get(4)?,
                content: row.get(5)?,
                created_at_ms: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let map = rows.into_iter().map(|s| (s.session_id, s)).collect();
    Ok(map)
}

/// Delete all summary rows for a session. Used before regenerating.
pub fn delete_summaries_for_session(
    conn: &Connection,
    session_id: i64,
) -> Result<usize, StorageError> {
    let n = conn.execute(
        "DELETE FROM summaries WHERE session_id = ?1",
        params![session_id],
    )?;
    Ok(n)
}
