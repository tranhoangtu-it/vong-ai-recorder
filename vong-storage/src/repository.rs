//! CRUD operations.
use crate::error::StorageError;
use crate::models::{normalize, NewSegment, NewSession, Segment, Session};
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
