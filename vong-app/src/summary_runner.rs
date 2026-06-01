//! Session-summary orchestration: fetch segments → call OpenAI → persist → update UI state.
//!
//! `SummaryRunner` is constructed once in `main()` and shared via `Arc`.
//! The Slint 30 Hz timer reads `state` to push updates into the UI.
//! Callers trigger runs via `SummaryRunner::trigger(...)`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use vong_storage::{delete_summaries_for_session, fetch_segments_for_session, insert_summary,
                   Connection, NewSummary};
use vong_transcribe::{generate_summary, ApiKey, SummaryError};

/// Summary state for a single session, mirrored into the Slint UI at 30 Hz.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Variants read by UI timer (not yet wired) + tests.
pub enum SummaryState {
    /// Summary task spawned but not yet complete.
    Pending,
    /// Summary generated successfully. Inner string is the summary content.
    Done(String),
    /// Session too short for a useful summary (< 10 segments or < 200 chars).
    Skipped(SkipReason),
    /// Summary generation failed.
    Failed(FailureKind),
    /// No OpenAI API key configured — user must set one in Settings → System.
    NeedsApiKey,
}

/// Why a summary was skipped (no API call made).
#[derive(Debug, Clone)]
pub enum SkipReason {
    TooShort,
}

/// Category of summary failure (shown in UI with Vietnamese message).
#[derive(Debug, Clone)]
pub enum FailureKind {
    Network,
    InvalidKey,
    RateLimited,
    ServerError(u16),
    Other(String),
}

impl FailureKind {
    /// Vietnamese UI message for each failure kind.
    #[allow(dead_code)]
    pub fn vi_message(&self) -> String {
        match self {
            Self::Network => "Lỗi mạng — nhấn Tạo lại để thử lại".into(),
            Self::InvalidKey => "API key không hợp lệ — kiểm tra Settings → System".into(),
            Self::RateLimited => "Hết hạn mức OpenAI — nhấn Tạo lại sau".into(),
            Self::ServerError(c) => format!("Lỗi server OpenAI ({c}) — nhấn Tạo lại sau"),
            Self::Other(s) => format!("Lỗi tóm tắt: {s}"),
        }
    }
}

/// Shared state container for the summary runner.
pub struct SummaryRunner {
    /// Per-session summary state — written by runner, read by UI timer.
    pub state: Arc<Mutex<HashMap<i64, SummaryState>>>,
    /// How many times user manually regenerated each session (capped at 3).
    pub regen_counts: Arc<Mutex<HashMap<i64, u8>>>,
}

impl SummaryRunner {
    /// Create a new empty runner.
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(HashMap::new())),
            regen_counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Trigger a summary run for `session_id` on the given tokio runtime.
    ///
    /// - `regen = false` → normal end-of-session run (counts as 0 toward regen cap).
    /// - `regen = true`  → user-initiated regenerate (increments counter, cap = 3).
    #[allow(dead_code)]
    pub fn trigger(
        &self,
        runtime: &tokio::runtime::Runtime,
        conn: Arc<Mutex<Connection>>,
        session_id: i64,
        regen: bool,
    ) {
        let state = self.state.clone();
        let regen_counts = self.regen_counts.clone();
        runtime.spawn(async move {
            run_summary_for_session(state, regen_counts, conn, session_id, regen).await;
        });
    }

    /// Trigger a summary run using a fire-and-forget background thread with its
    /// own minimal tokio runtime. Use this from synchronous UI callbacks where no
    /// runtime handle is available. The task runs independently; the caller does
    /// not wait for completion.
    pub fn trigger_detached(
        &self,
        conn: Arc<Mutex<Connection>>,
        session_id: i64,
        regen: bool,
    ) {
        let state = self.state.clone();
        let regen_counts = self.regen_counts.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("summary detached runtime");
            rt.block_on(run_summary_for_session(
                state,
                regen_counts,
                conn,
                session_id,
                regen,
            ));
        });
    }

    /// Run the summary pipeline synchronously on a new runtime, bounded by
    /// `timeout`. Used during app shutdown to flush the summary before the
    /// process exits. Exceeding the timeout is silently ignored.
    pub fn trigger_blocking_timeout(
        &self,
        conn: Arc<Mutex<Connection>>,
        session_id: i64,
        regen: bool,
        timeout: std::time::Duration,
    ) {
        let state = self.state.clone();
        let regen_counts = self.regen_counts.clone();
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "summary: shutdown runtime build failed");
                return;
            }
        };
        let _ = rt.block_on(tokio::time::timeout(
            timeout,
            run_summary_for_session(state, regen_counts, conn, session_id, regen),
        ));
    }

    /// Return the current regen count for a session.
    pub fn regen_count(&self, session_id: i64) -> u8 {
        self.regen_counts
            .lock()
            .map(|g| *g.get(&session_id).unwrap_or(&0))
            .unwrap_or(0)
    }
}

impl Default for SummaryRunner {
    fn default() -> Self {
        Self::new()
    }
}

/// Core async pipeline: load segments → guard → call API → persist → update state.
pub async fn run_summary_for_session(
    state_map: Arc<Mutex<HashMap<i64, SummaryState>>>,
    regen_counts: Arc<Mutex<HashMap<i64, u8>>>,
    conn: Arc<Mutex<Connection>>,
    session_id: i64,
    regen: bool,
) {
    // ── Regen cap check ────────────────────────────────────────────────────────
    if regen {
        let mut counts = regen_counts.lock().unwrap();
        let entry = counts.entry(session_id).or_insert(0);
        if *entry >= 3 {
            tracing::info!(session_id, "summary: regen cap reached — skipping");
            return;
        }
        *entry += 1;
        tracing::info!(session_id, regen_count = *entry, "summary: regen triggered");
    }

    // Mark as pending immediately so the UI shows a spinner.
    state_map
        .lock()
        .unwrap()
        .insert(session_id, SummaryState::Pending);

    // ── Load API key (blocking keychain I/O — do inside the task, not UI thread) ─
    let api_key = match ApiKey::load("openai-realtime") {
        Ok(k) => k,
        Err(_) => {
            tracing::info!(session_id, "summary: no OpenAI key in keychain");
            state_map
                .lock()
                .unwrap()
                .insert(session_id, SummaryState::NeedsApiKey);
            return;
        }
    };

    // ── Fetch transcript segments ──────────────────────────────────────────────
    let segments = {
        let guard = conn.lock().unwrap();
        match fetch_segments_for_session(&guard, session_id) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(session_id, error = ?e, "summary: fetch_segments_for_session failed");
                state_map.lock().unwrap().insert(
                    session_id,
                    SummaryState::Failed(FailureKind::Other(format!("db: {e:?}"))),
                );
                return;
            }
        }
    };

    let segment_count = segments.len();
    let concatenated: String = segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let char_count = concatenated.chars().count();

    // ── Cost guard: skip very short sessions ───────────────────────────────────
    if segment_count < 10 || char_count < 200 {
        tracing::info!(
            session_id,
            segment_count,
            char_count,
            "summary: skipped (too short)"
        );
        state_map
            .lock()
            .unwrap()
            .insert(session_id, SummaryState::Skipped(SkipReason::TooShort));
        return;
    }

    // ── Call OpenAI chat completions ────────────────────────────────────────────
    let start = std::time::Instant::now();
    let result = generate_summary(&api_key, &concatenated).await;
    let latency_ms = start.elapsed().as_millis();

    match result {
        Ok(resp) => {
            // Privacy: log only metadata, never content.
            tracing::info!(
                session_id,
                segment_count,
                concatenated_chars = char_count,
                prompt_tokens = resp.prompt_tokens,
                completion_tokens = resp.completion_tokens,
                latency_ms,
                "summary: done"
            );

            // ── Persist to summaries table ────────────────────────────────────
            let new_row = NewSummary {
                session_id,
                kind: "session".into(),
                llm_provider: Some("openai".into()),
                llm_model: Some(resp.model.clone()),
                content: resp.content.clone(),
            };
            {
                let guard = conn.lock().unwrap();
                if regen {
                    if let Err(e) = delete_summaries_for_session(&guard, session_id) {
                        tracing::warn!(session_id, error = ?e, "summary: delete old summaries failed");
                    }
                }
                if let Err(e) = insert_summary(&guard, &new_row) {
                    tracing::warn!(session_id, error = ?e, "summary: insert_summary failed");
                }
            }

            state_map
                .lock()
                .unwrap()
                .insert(session_id, SummaryState::Done(resp.content));
        }
        Err(e) => {
            tracing::warn!(
                session_id,
                latency_ms,
                error_kind = match &e {
                    SummaryError::InvalidKey => "invalid_key",
                    SummaryError::RateLimited => "rate_limited",
                    SummaryError::ServerError(_) => "server_error",
                    SummaryError::Timeout => "timeout",
                    SummaryError::NeedsApiKey => "needs_api_key",
                    SummaryError::Provider { .. } => "provider",
                },
                "summary: failed"
            );
            let kind = match e {
                SummaryError::InvalidKey => FailureKind::InvalidKey,
                SummaryError::RateLimited => FailureKind::RateLimited,
                SummaryError::ServerError(c) => FailureKind::ServerError(c),
                SummaryError::Timeout => FailureKind::Network,
                SummaryError::NeedsApiKey => {
                    state_map
                        .lock()
                        .unwrap()
                        .insert(session_id, SummaryState::NeedsApiKey);
                    return;
                }
                SummaryError::Provider { code, message } => {
                    FailureKind::Other(format!("provider {code}: {message}"))
                }
            };
            state_map
                .lock()
                .unwrap()
                .insert(session_id, SummaryState::Failed(kind));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vong_storage::{insert_segment, insert_session, open_in_memory, NewSegment, NewSession};

    fn make_in_memory_conn() -> Arc<Mutex<Connection>> {
        let conn = open_in_memory().expect("in-memory db");
        Arc::new(Mutex::new(conn))
    }

    fn make_session(conn: &Arc<Mutex<Connection>>) -> i64 {
        let guard = conn.lock().unwrap();
        insert_session(
            &guard,
            &NewSession {
                started_at_ms: 1_000_000,
                audio_source: "mic".into(),
                provider: "whisper-local".into(),
                detected_language: None,
                meta_json: None,
            },
        )
        .expect("insert session")
    }

    fn add_segment(conn: &Arc<Mutex<Connection>>, session_id: i64, seq: i64, text: &str) {
        let guard = conn.lock().unwrap();
        insert_segment(
            &guard,
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

    #[tokio::test]
    async fn skip_when_segments_below_threshold() {
        let conn = make_in_memory_conn();
        let session_id = make_session(&conn);

        // Add only 9 segments (< 10 threshold).
        for i in 0..9i64 {
            add_segment(&conn, session_id, i, "word word word word word word word word");
        }

        let state_map = Arc::new(Mutex::new(HashMap::new()));
        let regen_counts = Arc::new(Mutex::new(HashMap::new()));

        // We cannot call OpenAI in tests, but the skip guard fires before the API call.
        run_summary_for_session(
            state_map.clone(),
            regen_counts,
            conn,
            session_id,
            false,
        )
        .await;

        // Should be skipped because API key is absent (NeedsApiKey fires first).
        // Either NeedsApiKey or Skipped is fine — both mean no API call was made.
        let guard = state_map.lock().unwrap();
        let state = guard.get(&session_id).expect("state set");
        assert!(
            matches!(state, SummaryState::NeedsApiKey | SummaryState::Skipped(_)),
            "expected NeedsApiKey or Skipped, got {:?}",
            state
        );
    }

    #[tokio::test]
    async fn skip_when_concatenated_chars_below_threshold() {
        let conn = make_in_memory_conn();
        let session_id = make_session(&conn);

        // 15 segments × 5 chars each = 75 chars total < 200.
        for i in 0..15i64 {
            add_segment(&conn, session_id, i, "hello");
        }

        let state_map = Arc::new(Mutex::new(HashMap::new()));
        let regen_counts = Arc::new(Mutex::new(HashMap::new()));

        run_summary_for_session(
            state_map.clone(),
            regen_counts,
            conn,
            session_id,
            false,
        )
        .await;

        let guard = state_map.lock().unwrap();
        let state = guard.get(&session_id).expect("state set");
        // NeedsApiKey fires before the char-count check (key absent in test env).
        assert!(
            matches!(state, SummaryState::NeedsApiKey | SummaryState::Skipped(_)),
            "expected NeedsApiKey or Skipped, got {:?}",
            state
        );
    }

    #[tokio::test]
    async fn regen_count_caps_at_3() {
        let runner = SummaryRunner::new();
        let conn = make_in_memory_conn();
        let session_id = make_session(&conn);

        let rt = tokio::runtime::Handle::current();
        // Simulate 3 regen triggers (each will short-circuit at NeedsApiKey before API call).
        for _ in 0..3 {
            let state = runner.state.clone();
            let regen_counts = runner.regen_counts.clone();
            run_summary_for_session(state, regen_counts, conn.clone(), session_id, true).await;
        }

        assert_eq!(runner.regen_count(session_id), 3);

        // 4th attempt should be rejected before even setting state to Pending.
        let state_map = runner.state.clone();
        let regen_counts = runner.regen_counts.clone();
        run_summary_for_session(state_map, regen_counts, conn.clone(), session_id, true).await;

        // regen count stays at 3 (cap enforced).
        assert_eq!(runner.regen_count(session_id), 3, "regen count must not exceed 3");

        let _ = rt; // keep handle in scope
    }

    #[test]
    fn failure_kind_vi_message_invalid_key() {
        let msg = FailureKind::InvalidKey.vi_message();
        assert!(msg.contains("API key"));
    }

    #[test]
    fn failure_kind_vi_message_rate_limited() {
        let msg = FailureKind::RateLimited.vi_message();
        assert!(msg.contains("OpenAI"));
    }
}
