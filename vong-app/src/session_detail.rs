//! Session detail loader and clipboard helper for the SessionDetail view.
//!
//! Loads a session, its summary, and its transcript segments together in one
//! place so the SessionDetail view has everything it needs without N+1 queries.

use std::sync::{Arc, Mutex};
use vong_storage::{Connection, Segment, Session, StorageError, Summary};

/// All data needed to render the SessionDetail view.
#[derive(Debug, Clone)]
pub struct SessionDetailLoad {
    pub session: Session,
    pub summary: Option<Summary>,
    pub segments: Vec<Segment>,
}

impl SessionDetailLoad {
    /// Load a full session detail from an open DB connection.
    ///
    /// Returns `Err` only when the session row itself is missing or a DB error
    /// occurs. Missing summary is represented as `summary: None` (not an error).
    pub fn from_db(conn: &Connection, session_id: i64) -> Result<Self, StorageError> {
        let session = vong_storage::fetch_session(conn, session_id)?;
        let summary = vong_storage::fetch_latest_summary(conn, session_id)?;
        let segments = vong_storage::fetch_segments_for_session(conn, session_id)?;
        Ok(Self {
            session,
            summary,
            segments,
        })
    }
}

/// Format a summary content string for clipboard copy.
///
/// Prepends a minimal header so the pasted text is self-contained.
/// Returns the formatted string — the actual clipboard write is left to
/// the caller so tests can verify the formatted text without a real clipboard.
pub fn format_summary_for_clipboard(detail: &SessionDetailLoad) -> String {
    let content = match &detail.summary {
        Some(s) => s.content.clone(),
        None => return String::new(),
    };
    let started = format_timestamp_ms(detail.session.started_at_ms);
    format!("Tóm tắt phiên #{id} ({started})\n\n{content}", id = detail.session.id)
}

/// Write text to the system clipboard via arboard. Returns an error string
/// (for logging) on failure — never panics.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    arboard::Clipboard::new()
        .map_err(|e| format!("clipboard init: {e}"))?
        .set_text(text.to_string())
        .map_err(|e| format!("clipboard set_text: {e}"))
}

/// Format epoch-millisecond timestamp as "YYYY-MM-DD HH:MM" (local time).
/// Falls back to raw ms on conversion failure.
pub fn format_timestamp_ms(ms: i64) -> String {
    use std::time::{Duration, UNIX_EPOCH};
    let secs = ms / 1000;
    let sub_ms = (ms % 1000) as u32;
    let sys_time = UNIX_EPOCH + Duration::new(secs as u64, sub_ms * 1_000_000);
    // Use chrono-free formatting: convert to a UTC-offset naive string.
    // We only need a readable label — local offset precision is secondary.
    match sys_time.duration_since(UNIX_EPOCH) {
        Ok(d) => {
            // Derive YMD from unix epoch (UTC, good enough for a log label).
            let total_secs = d.as_secs();
            let days = total_secs / 86400;
            let time_of_day = total_secs % 86400;
            let h = time_of_day / 3600;
            let m = (time_of_day % 3600) / 60;
            // Gregorian calendar conversion (Zeller-style)
            let (y, mo, da) = days_to_ymd(days);
            format!("{y:04}-{mo:02}-{da:02} {h:02}:{m:02}")
        }
        Err(_) => format!("{ms}ms"),
    }
}

/// Convert days-since-epoch (1970-01-01 = 0) to (year, month, day).
fn days_to_ymd(z: u64) -> (u64, u64, u64) {
    let z = z + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    (y, mo, d)
}

/// Build the `HistoryRowData` summary preview string from an optional summary.
///
/// - `None` → "Chưa tóm tắt"
/// - Content ≤ 80 chars → content as-is
/// - Content > 80 chars → first 80 chars + "…"
pub fn summary_preview(summary: Option<&Summary>) -> String {
    match summary {
        None => "Chưa tóm tắt".to_string(),
        Some(s) => {
            let chars: Vec<char> = s.content.chars().collect();
            if chars.len() <= 80 {
                s.content.clone()
            } else {
                let truncated: String = chars[..80].iter().collect();
                format!("{truncated}…")
            }
        }
    }
}

/// Format session duration as "Xm Ys" or "Xs" string for HistoryCard rows.
pub fn format_duration(duration_ms: Option<i64>) -> String {
    match duration_ms {
        None | Some(0) => "—".to_string(),
        Some(ms) => {
            let secs = (ms / 1000) as u64;
            if secs >= 60 {
                format!("{}m {}s", secs / 60, secs % 60)
            } else {
                format!("{secs}s")
            }
        }
    }
}

/// Thread-safe wrapper around `SessionDetailLoad` used as shared UI state.
pub type SharedDetail = Arc<Mutex<Option<SessionDetailLoad>>>;

/// Create an empty shared detail cell.
pub fn empty_shared_detail() -> SharedDetail {
    Arc::new(Mutex::new(None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vong_storage::{
        insert_segment, insert_session, insert_summary, open_in_memory, NewSegment, NewSession,
        NewSummary,
    };

    fn make_conn() -> Connection {
        open_in_memory().expect("in-memory db")
    }

    fn make_session(conn: &Connection) -> i64 {
        insert_session(
            conn,
            &NewSession {
                started_at_ms: 1_748_000_000_000, // 2025-05-23 approx
                audio_source: "mic".into(),
                provider: "whisper-local".into(),
                detected_language: Some("vi".into()),
                meta_json: None,
            },
        )
        .expect("insert session")
    }

    fn add_segment(conn: &Connection, session_id: i64, seq: i64, text: &str) {
        insert_segment(
            conn,
            &NewSegment {
                session_id,
                seq,
                start_ms: seq * 500,
                end_ms: seq * 500 + 500,
                speaker_label: None,
                text: text.into(),
                language: Some("vi".into()),
                confidence: None,
                is_final: true,
            },
        )
        .expect("insert segment");
    }

    // Test 1: from_db loads session + segments + summary correctly.
    #[test]
    fn from_db_loads_all_fields() {
        let conn = make_conn();
        let sid = make_session(&conn);
        add_segment(&conn, sid, 0, "Xin chào");
        add_segment(&conn, sid, 1, "Cuộc họp hôm nay");
        add_segment(&conn, sid, 2, "Tóm tắt cuối");
        insert_summary(
            &conn,
            &NewSummary {
                session_id: sid,
                kind: "session".into(),
                llm_provider: Some("openai".into()),
                llm_model: Some("gpt-4o-mini".into()),
                content: "Tóm tắt phiên thử nghiệm.".into(),
            },
        )
        .expect("insert summary");

        let detail = SessionDetailLoad::from_db(&conn, sid).expect("from_db");
        assert_eq!(detail.session.id, sid);
        assert_eq!(detail.segments.len(), 3);
        assert!(detail.summary.is_some());
        assert!(detail.summary.unwrap().content.contains("thử nghiệm"));
    }

    // Test 2: summary_preview returns "Chưa tóm tắt" when None; truncates at 80 chars.
    #[test]
    fn summary_preview_none_and_truncation() {
        assert_eq!(summary_preview(None), "Chưa tóm tắt");

        let short = Summary {
            id: 1,
            session_id: 1,
            kind: "session".into(),
            llm_provider: None,
            llm_model: None,
            content: "Short content.".into(),
            created_at_ms: 0,
        };
        assert_eq!(summary_preview(Some(&short)), "Short content.");

        let long_content: String = "A".repeat(100);
        let long_sum = Summary {
            content: long_content.clone(),
            ..short.clone()
        };
        let preview = summary_preview(Some(&long_sum));
        // Should be first 80 chars + "…"
        assert!(preview.ends_with('…'), "preview must end with ellipsis");
        // Character count: 80 + 1 (ellipsis) = 81
        assert_eq!(preview.chars().count(), 81);
    }

    // Test 3: batched summary fetch — 5 sessions, 3 have summaries.
    #[test]
    fn fetch_summaries_for_sessions_batched() {
        let conn = make_conn();
        let ids: Vec<i64> = (0..5).map(|_| make_session(&conn)).collect();
        // Insert summaries for sessions 0, 1, 2 only.
        for &sid in &ids[..3] {
            insert_summary(
                &conn,
                &NewSummary {
                    session_id: sid,
                    kind: "session".into(),
                    llm_provider: None,
                    llm_model: None,
                    content: format!("Summary for session {sid}"),
                },
            )
            .expect("insert summary");
        }
        let map = vong_storage::fetch_summaries_for_sessions(&conn, &ids)
            .expect("fetch_summaries_for_sessions");
        assert_eq!(map.len(), 3, "only 3 out of 5 sessions have summaries");
        for &sid in &ids[..3] {
            assert!(map.contains_key(&sid));
        }
        for &sid in &ids[3..] {
            assert!(!map.contains_key(&sid));
        }
    }

    // Test 4: format_summary_for_clipboard produces correct text structure.
    #[test]
    fn clipboard_format_contains_session_id_and_content() {
        let conn = make_conn();
        let sid = make_session(&conn);
        insert_summary(
            &conn,
            &NewSummary {
                session_id: sid,
                kind: "session".into(),
                llm_provider: None,
                llm_model: None,
                content: "Nội dung tóm tắt kiểm tra.".into(),
            },
        )
        .expect("insert");
        let detail = SessionDetailLoad::from_db(&conn, sid).expect("load");
        let text = format_summary_for_clipboard(&detail);
        assert!(text.contains(&format!("#{sid}")), "must contain session id");
        assert!(text.contains("Nội dung tóm tắt kiểm tra."), "must contain summary content");
    }
}
