//! Transcript export to text formats — txt, markdown, srt.
use crate::error::StorageError;
use crate::repository::{fetch_segments_for_session, fetch_session};
use rusqlite::Connection;

/// Export session as plain text. Segments joined with newlines.
pub fn export_text(conn: &Connection, session_id: i64) -> Result<String, StorageError> {
    let segments = fetch_segments_for_session(conn, session_id)?;
    let body = segments
        .iter()
        .map(|s| s.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(body)
}

/// Export session as Markdown with timestamps + metadata header.
pub fn export_markdown(conn: &Connection, session_id: i64) -> Result<String, StorageError> {
    let session = fetch_session(conn, session_id)?;
    let segments = fetch_segments_for_session(conn, session_id)?;

    let mut out = String::new();
    out.push_str("# Transcript\n\n");
    out.push_str(&format!("**Session ID**: {}\n\n", session.id));
    out.push_str(&format!(
        "**Started at (epoch ms)**: {}\n\n",
        session.started_at_ms
    ));
    if let Some(dur) = session.duration_ms {
        out.push_str(&format!("**Duration**: {}s\n\n", dur / 1000));
    }
    out.push_str(&format!("**Audio source**: {}\n\n", session.audio_source));
    out.push_str(&format!("**Provider**: {}\n\n", session.provider));
    if let Some(lang) = &session.detected_language {
        out.push_str(&format!("**Language**: {}\n\n", lang));
    }
    out.push_str("---\n\n");
    for seg in &segments {
        out.push_str(&format!(
            "**[{}]** {}\n\n",
            format_ms_as_mmss(seg.start_ms),
            seg.text
        ));
    }
    Ok(out)
}

/// Export session as SRT subtitle (numbered cues with HH:MM:SS,mmm timestamps).
pub fn export_srt(conn: &Connection, session_id: i64) -> Result<String, StorageError> {
    let segments = fetch_segments_for_session(conn, session_id)?;
    let mut out = String::new();
    for (idx, seg) in segments.iter().enumerate() {
        out.push_str(&format!("{}\n", idx + 1));
        out.push_str(&format!(
            "{} --> {}\n",
            format_srt_timestamp(seg.start_ms),
            format_srt_timestamp(seg.end_ms)
        ));
        out.push_str(&seg.text);
        out.push_str("\n\n");
    }
    Ok(out)
}

fn format_ms_as_mmss(ms: i64) -> String {
    let total_s = ms / 1000;
    let m = total_s / 60;
    let s = total_s % 60;
    format!("{:02}:{:02}", m, s)
}

fn format_srt_timestamp(ms: i64) -> String {
    let total_s = ms / 1000;
    let h = total_s / 3600;
    let m = (total_s % 3600) / 60;
    let s = total_s % 60;
    let ms_part = ms % 1000;
    format!("{:02}:{:02}:{:02},{:03}", h, m, s, ms_part)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;
    use crate::models::{NewSegment, NewSession};
    use crate::repository::{finalize_session, insert_segment, insert_session};

    fn setup() -> (Connection, i64) {
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
        for (i, text) in ["Hello world", "How are you", "Goodbye"].iter().enumerate() {
            insert_segment(
                &conn,
                &NewSegment {
                    session_id: sid,
                    seq: i as i64,
                    start_ms: (i as i64) * 2000,
                    end_ms: (i as i64 + 1) * 2000,
                    speaker_label: None,
                    text: text.to_string(),
                    language: Some("en".into()),
                    confidence: None,
                    is_final: true,
                },
            )
            .unwrap();
        }
        finalize_session(&conn, sid).unwrap();
        (conn, sid)
    }

    #[test]
    fn export_text_joins_segments() {
        let (conn, sid) = setup();
        let txt = export_text(&conn, sid).unwrap();
        assert!(txt.contains("Hello world"));
        assert!(txt.contains("Goodbye"));
        assert!(txt.lines().count() >= 3);
    }

    #[test]
    fn export_markdown_includes_metadata() {
        let (conn, sid) = setup();
        let md = export_markdown(&conn, sid).unwrap();
        assert!(md.contains("# Transcript"));
        assert!(md.contains("**Provider**: soniox"));
        assert!(md.contains("**Language**: vi"));
        assert!(md.contains("Hello world"));
    }

    #[test]
    fn export_srt_format() {
        let (conn, sid) = setup();
        let srt = export_srt(&conn, sid).unwrap();
        assert!(srt.contains("1\n"));
        assert!(srt.contains("00:00:00,000 --> 00:00:02,000"));
        assert!(srt.contains("Hello world"));
    }

    #[test]
    fn srt_timestamp_format() {
        assert_eq!(format_srt_timestamp(0), "00:00:00,000");
        assert_eq!(format_srt_timestamp(1234), "00:00:01,234");
        assert_eq!(format_srt_timestamp(3_661_500), "01:01:01,500");
    }

    #[test]
    fn mmss_format() {
        assert_eq!(format_ms_as_mmss(0), "00:00");
        assert_eq!(format_ms_as_mmss(65_000), "01:05");
    }
}
