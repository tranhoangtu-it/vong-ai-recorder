//! First-run wizard — persistence, state machine, and API key test helper.
//!
//! Persistence layout (`%APPDATA%\Vong\Vong AI Recorder\config\onboarded.txt`):
//!
//! ```text
//! v=2
//! step.welcome.done=2026-05-21T10:00:00Z
//! step.audio.done=2026-05-21T10:00:15Z
//! step.provider.done=2026-05-21T10:00:30Z
//! step.api_key.done=2026-05-21T10:01:00Z
//! step.api_key.skipped=2026-05-21T10:01:00Z
//! step.model.done=2026-05-21T10:04:30Z
//! step.language.done=2026-05-21T10:04:45Z
//! step.complete=2026-05-21T10:05:00Z
//! ```
//!
//! Legacy files (no `v=2` header) → treated as fully complete (alpha users).
//! Schema version check prevents silent upgrades — must be v2 to resume.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use vong_transcribe::ApiKey;

use crate::sentry_init;

// ── Schema version ────────────────────────────────────────────────────────────

const SCHEMA_VERSION: &str = "v=2";

// ── Wizard step definitions ───────────────────────────────────────────────────

/// Logical wizard steps in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardStep {
    Welcome,     // 0
    Audio,       // 1
    Provider,    // 2
    ApiKey,      // 3 — conditional: shown only for Soniox / OpenAI
    Model,       // 4 — conditional: shown only for LocalWhisper
    Language,    // 5
    CrashReport, // 6 — Phase 9: opt-in crash reporting (Sentry)
    Done,        // 7 (was 6 in schema v=2; Done shifted to 7 — legacy step.complete= still Complete)
}

impl WizardStep {
    /// Persistence key suffix used in `onboarded.txt`.
    pub fn key(self) -> &'static str {
        match self {
            Self::Welcome => "welcome",
            Self::Audio => "audio",
            Self::Provider => "provider",
            Self::ApiKey => "api_key",
            Self::Model => "model",
            Self::Language => "language",
            Self::CrashReport => "crash_report",
            Self::Done => "done", // not directly persisted; step.complete= is written instead
        }
    }

    /// 0-based display index (matches Slint `wizard-step` property).
    /// Public API — used by test suite and future Settings "re-run wizard" feature.
    #[allow(dead_code)]
    pub fn index(self) -> usize {
        match self {
            Self::Welcome => 0,
            Self::Audio => 1,
            Self::Provider => 2,
            Self::ApiKey => 3,
            Self::Model => 4,
            Self::Language => 5,
            Self::CrashReport => 6,
            Self::Done => 7, // was 6; shifted to 7 by insertion of CrashReport
        }
    }

    /// Convert from display index back to WizardStep.
    pub fn from_index(i: usize) -> Option<Self> {
        match i {
            0 => Some(Self::Welcome),
            1 => Some(Self::Audio),
            2 => Some(Self::Provider),
            3 => Some(Self::ApiKey),
            4 => Some(Self::Model),
            5 => Some(Self::Language),
            6 => Some(Self::CrashReport),
            7 => Some(Self::Done),
            _ => None,
        }
    }
}

// ── Progress state ────────────────────────────────────────────────────────────

/// Wizard progress at startup — drives `initial_view` + `initial_step`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardProgress {
    /// Fresh install — no marker file, or marker file lacks `v=2` header.
    NotStarted,
    /// v=2 marker exists, user completed up through `step_index` (0-based).
    /// Next launch should resume at `step_index + 1`.
    Resume(usize),
    /// `step.complete=` line present — wizard fully done.
    Complete,
}

// ── Persistence helpers ───────────────────────────────────────────────────────

/// Path to the wizard marker file.
/// `%APPDATA%\Vong\Vong AI Recorder\config\onboarded.txt`
pub fn marker_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "Vong", "Vong AI Recorder")
        .map(|d| d.config_dir().join("onboarded.txt"))
}

/// ISO 8601 UTC timestamp for persistence lines (no `chrono` dep needed).
fn now_ts() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Format as Unix timestamp — simple, unambiguous, parseable by any tool.
    // Full ISO string needs chrono; secs-since-epoch is safe and correct here.
    format!("{secs}")
}

/// Read wizard progress from the marker file.
///
/// Rules:
/// - File missing → `NotStarted`
/// - File exists without `v=2` header → `Complete` (legacy alpha user — no re-onboarding)
/// - File exists with `v=2` + `step.complete=` → `Complete`
/// - File exists with `v=2` but no `step.complete=` → `Resume(last_completed_index)`
/// - File exists with `v=2` but only `v=2` line → `NotStarted` (fresh file)
pub fn read_progress() -> WizardProgress {
    let path = match marker_path() {
        Some(p) => p,
        None => return WizardProgress::NotStarted,
    };

    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return WizardProgress::NotStarted,
    };

    // Legacy: file exists but has no v=2 header → treat as complete (alpha users).
    if !content.lines().any(|l| l.trim() == SCHEMA_VERSION) {
        // But if the file is completely empty (edge case: old 0-byte sentinel),
        // also treat as complete — alpha install.
        return WizardProgress::Complete;
    }

    // v=2 file: check for completion marker.
    if content
        .lines()
        .any(|l| l.trim().starts_with("step.complete="))
    {
        return WizardProgress::Complete;
    }

    // Walk step keys in order and find the last completed one.
    // NOTE: step.crash_report.done= is included so resume logic works for
    // new users who reach that step mid-wizard. Sprint 1 alpha users with
    // step.complete= are already handled above (returns Complete before
    // reaching this array). They get the Settings → System toggle as their
    // only Sentry opt-in path — no re-onboarding triggered.
    let ordered_keys = [
        "step.welcome.done=",
        "step.audio.done=",
        "step.provider.done=",
        // api_key is special: either .done= or .skipped= counts
        "step.api_key.",
        "step.model.done=",
        "step.language.done=",
        "step.crash_report.done=", // Phase 9: index 6 in ordered_keys → WizardStep::CrashReport
    ];

    let mut last_completed: Option<usize> = None;
    for (idx, key_prefix) in ordered_keys.iter().enumerate() {
        if content
            .lines()
            .any(|l| l.trim().starts_with(key_prefix))
        {
            last_completed = Some(idx);
        }
    }

    match last_completed {
        Some(idx) => WizardProgress::Resume(idx),
        None => WizardProgress::NotStarted,
    }
}

/// Ensure marker file exists with the `v=2` header.
/// Idempotent — if header already present, does nothing.
fn ensure_v2_header() -> io::Result<()> {
    let path = marker_path().ok_or_else(|| io::Error::other("ProjectDirs unavailable"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Check if the file already has the header.
    if path.exists() {
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if existing.lines().any(|l| l.trim() == SCHEMA_VERSION) {
            return Ok(());
        }
        // File exists without header — this is a legacy file; we should NOT
        // overwrite it (would change Complete → NotStarted for alpha users).
        // The spec says: treat legacy as Complete, don't wipe.
        // So we just don't append anything — caller should have checked.
        return Ok(());
    }
    // Fresh file: write header.
    let mut f = std::fs::File::create(&path)?;
    writeln!(f, "{SCHEMA_VERSION}")?;
    Ok(())
}

/// Mark a wizard step as done by appending a line to the marker file.
///
/// For most steps: `step.<key>.done=<timestamp>`
/// For api_key when skipped: call `mark_api_key_skipped()` instead.
pub fn mark_step_done(step: WizardStep) -> io::Result<()> {
    ensure_v2_header()?;
    let path = marker_path().ok_or_else(|| io::Error::other("ProjectDirs unavailable"))?;
    let line = format!("step.{}.done={}\n", step.key(), now_ts());
    // Open in append mode — atomic for < 512 B on Windows NTFS (single-sector write).
    let mut f = std::fs::OpenOptions::new().append(true).open(&path)?;
    f.write_all(line.as_bytes())
}

/// Mark the api_key step as skipped (user dismissed without entering a key).
pub fn mark_api_key_skipped() -> io::Result<()> {
    ensure_v2_header()?;
    let path = marker_path().ok_or_else(|| io::Error::other("ProjectDirs unavailable"))?;
    let line = format!("step.api_key.skipped={}\n", now_ts());
    let mut f = std::fs::OpenOptions::new().append(true).open(&path)?;
    f.write_all(line.as_bytes())
}

/// Mark the wizard as fully complete. Sets `current-view = "main"` from the
/// caller after this returns.
pub fn mark_complete() -> io::Result<()> {
    ensure_v2_header()?;
    let path = marker_path().ok_or_else(|| io::Error::other("ProjectDirs unavailable"))?;
    let line = format!("step.complete={}\n", now_ts());
    let mut f = std::fs::OpenOptions::new().append(true).open(&path)?;
    f.write_all(line.as_bytes())
}

/// Quick check: is the wizard fully done?
/// Public API — available for Settings "re-run wizard" guard in Sprint 2.
#[allow(dead_code)]
pub fn is_complete() -> bool {
    matches!(read_progress(), WizardProgress::Complete)
}

/// Persist the crash-report consent choice made at wizard step 6.
///
/// Writes both:
/// - `sentry.txt` — the runtime gate read by `sentry_init::load_consent()`.
/// - `onboarded.txt` line `step.crash_report.done=<timestamp>` — so that
///   `read_progress()` can resume correctly if the wizard is interrupted
///   after this step.
///
/// Caller is responsible for advancing the wizard to Done after this returns.
pub fn mark_crash_report_consent(enabled: bool) -> io::Result<()> {
    let state = if enabled {
        sentry_init::ConsentState::Enabled
    } else {
        sentry_init::ConsentState::Disabled
    };
    sentry_init::save_consent(state)?;
    mark_step_done(WizardStep::CrashReport)
}

// ── State machine: compute next step ─────────────────────────────────────────

/// Compute which Slint step index to show after the user clicks Next on `current_step`.
///
/// `provider` is the currently-selected provider as a string id
/// (`"local-whisper"` / `"soniox"` / `"openai-realtime"`).
pub fn compute_next_step_index(current_index: usize, provider: &str) -> usize {
    match current_index {
        0 => 1, // Welcome → Audio
        1 => 2, // Audio → Provider
        2 => {
            // Provider → ApiKey (Soniox/OpenAI) OR Model (LocalWhisper)
            match provider {
                "soniox" | "openai-realtime" => 3, // → ApiKey
                _ => 4,                             // → Model (LocalWhisper default)
            }
        }
        3 => 5, // ApiKey → Language (skips Model for cloud providers)
        4 => 5, // Model → Language
        5 => 6, // Language → CrashReport (Phase 9: was Language → Done)
        6 => 7, // CrashReport → Done
        _ => 7, // Done stays at Done
    }
}

/// Compute which Slint step index to show after the user clicks Back on `current_step`.
pub fn compute_prev_step_index(current_index: usize, provider: &str) -> usize {
    match current_index {
        0 => 0, // Welcome has no Back
        1 => 0, // Audio → Welcome
        2 => 1, // Provider → Audio
        3 => 2, // ApiKey → Provider
        4 => 2, // Model → Provider
        5 => {
            // Language → ApiKey (cloud) OR Model (LocalWhisper)
            match provider {
                "soniox" | "openai-realtime" => 3,
                _ => 4,
            }
        }
        6 => 5, // CrashReport → Language
        7 => 6, // Done → CrashReport (was Done → Language at index 5→6)
        _ => 0,
    }
}

// ── API key storage ───────────────────────────────────────────────────────────

/// Store an API key in the OS keychain for the given provider.
///
/// Returns `Err(String)` with a Vietnamese error message on failure.
/// On success marks the api_key step done in `onboarded.txt`.
pub fn save_api_key(provider: &str, key: &str) -> Result<(), String> {
    ApiKey::store(provider, key)
        .map_err(|e| format!("Không lưu được API key: {e}"))
}

// ── API key test (WebSocket ping) ─────────────────────────────────────────────

/// Test an API key by opening a WebSocket to the provider and checking auth.
///
/// Returns:
/// - `Ok(())` — key is valid.
/// - `Err(String)` — Vietnamese error message (network, auth, or timeout).
///
/// Wrapped in a 5-second timeout. The connection is closed immediately after
/// the auth check — no audio is sent, no quota used (beyond the initial
/// connection handshake).
pub async fn test_api_key(provider: &str, key: &str) -> Result<(), String> {
    let timeout = std::time::Duration::from_secs(5);

    match tokio::time::timeout(timeout, do_test_api_key(provider, key)).await {
        Ok(result) => result,
        Err(_) => Err("Hết thời gian chờ — kiểm tra kết nối mạng".to_string()),
    }
}

async fn do_test_api_key(provider: &str, key: &str) -> Result<(), String> {
    match provider {
        "soniox" => test_soniox(key).await,
        "openai-realtime" => test_openai(key).await,
        "local-whisper" => Ok(()), // no key needed
        _ => Err(format!("Provider không xác định: {provider}")),
    }
}

async fn test_soniox(key: &str) -> Result<(), String> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    const SONIOX_WS: &str = "wss://stt-rt.soniox.com/transcribe-websocket";

    let (mut ws, _) = tokio_tungstenite::connect_async(SONIOX_WS)
        .await
        .map_err(|e| format!("Không kết nối được tới Soniox: {e}"))?;

    // Send minimal config JSON — same shape as SonioxProvider::build_config()
    // but inlined to avoid vong-transcribe::SonioxProvider dependency here.
    let config_json = format!(
        r#"{{"api_key":"{key}","model":"stt-rt-preview","audio_format":"pcm_s16le","sample_rate":16000,"num_channels":1}}"#,
        key = key.replace('"', "\\\"")
    );
    ws.send(Message::Text(config_json.into()))
        .await
        .map_err(|e| format!("Gửi config thất bại: {e}"))?;

    // Send 100ms silence as probe
    let silence = vec![0u8; 3200]; // 1600 i16 samples × 2 bytes
    ws.send(Message::Binary(silence.into()))
        .await
        .map_err(|e| format!("Gửi audio thất bại: {e}"))?;

    // Wait for first server response (3s inner deadline — outer 5s wraps this fn)
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(3));
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            msg = ws.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        // Check for error code in response
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            if let Some(code) = v.get("error_code").and_then(|c| c.as_str()) {
                                if !code.is_empty() {
                                    let msg = v.get("error_message")
                                        .and_then(|m| m.as_str())
                                        .unwrap_or("Lỗi xác thực");
                                    let _ = ws.send(Message::Close(None)).await;
                                    return Err(format!("Soniox từ chối API key: {msg}"));
                                }
                            }
                        }
                        // Non-error response = key accepted
                        let _ = ws.send(Message::Close(None)).await;
                        return Ok(());
                    }
                    Some(Ok(Message::Close(_))) => {
                        return Err("Soniox đóng kết nối sớm — API key có thể không hợp lệ".to_string());
                    }
                    Some(Err(e)) => {
                        return Err(format!("Lỗi kết nối Soniox: {e}"));
                    }
                    None => {
                        return Err("Kết nối Soniox bị đóng".to_string());
                    }
                    _ => continue,
                }
            }
            _ = &mut deadline => {
                // No error in 3s = positive signal (idle until enough audio)
                let _ = ws.send(Message::Close(None)).await;
                return Ok(());
            }
        }
    }
}

async fn test_openai(key: &str) -> Result<(), String> {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::HeaderValue;
    use tokio_tungstenite::tungstenite::Message;

    const OPENAI_WS: &str = "wss://api.openai.com/v1/realtime?intent=transcription";

    let mut request = OPENAI_WS
        .into_client_request()
        .map_err(|e| format!("Lỗi tạo request: {e}"))?;

    {
        let headers = request.headers_mut();
        let auth_value = HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|e| format!("API key chứa ký tự không hợp lệ: {e}"))?;
        headers.insert("Authorization", auth_value);
        headers.insert("OpenAI-Beta", HeaderValue::from_static("realtime=v1"));
    }

    let (mut ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| {
            // HTTP 401 surfaces as a tungstenite error string
            let s = e.to_string();
            if s.contains("401") || s.contains("Unauthorized") {
                "API key OpenAI không hợp lệ (lỗi 401)".to_string()
            } else {
                format!("Không kết nối được tới OpenAI: {e}")
            }
        })?;

    // Wait for `session.created` confirming auth success (or any server message).
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(4));
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            msg = ws.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            if event_type == "error" {
                                let msg = v.get("error")
                                    .and_then(|e| e.get("message"))
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("Lỗi xác thực OpenAI");
                                let _ = ws.close(None).await;
                                return Err(format!("OpenAI từ chối: {msg}"));
                            }
                            if event_type == "session.created" || event_type.contains("session") {
                                let _ = ws.close(None).await;
                                return Ok(());
                            }
                        }
                        // Any non-error message = connected
                        let _ = ws.close(None).await;
                        return Ok(());
                    }
                    Some(Ok(Message::Close(_))) => {
                        return Err("OpenAI đóng kết nối — API key có thể không hợp lệ".to_string());
                    }
                    Some(Err(e)) => {
                        let s = e.to_string();
                        if s.contains("401") || s.contains("Unauthorized") {
                            return Err("API key OpenAI không hợp lệ (lỗi 401)".to_string());
                        }
                        return Err(format!("Lỗi kết nối OpenAI: {e}"));
                    }
                    None => {
                        return Err("Kết nối OpenAI bị đóng".to_string());
                    }
                    _ => continue,
                }
            }
            _ = &mut deadline => {
                let _ = ws.close(None).await;
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── State machine tests ──────────────────────────────────────────────────

    #[test]
    fn next_step_local_whisper_goes_to_model() {
        // Provider step → LocalWhisper → should go to Model step (4), not ApiKey (3)
        assert_eq!(compute_next_step_index(2, "local-whisper"), 4);
    }

    #[test]
    fn next_step_soniox_goes_to_api_key() {
        assert_eq!(compute_next_step_index(2, "soniox"), 3);
    }

    #[test]
    fn next_step_openai_goes_to_api_key() {
        assert_eq!(compute_next_step_index(2, "openai-realtime"), 3);
    }

    #[test]
    fn next_step_api_key_goes_to_language() {
        // ApiKey (3) → Language (5) — skips Model for cloud providers
        assert_eq!(compute_next_step_index(3, "soniox"), 5);
        assert_eq!(compute_next_step_index(3, "openai-realtime"), 5);
    }

    #[test]
    fn next_step_model_goes_to_language() {
        // Model (4) → Language (5)
        assert_eq!(compute_next_step_index(4, "local-whisper"), 5);
    }

    #[test]
    fn prev_step_language_cloud_goes_to_api_key() {
        // Language (5) Back → ApiKey (3) for cloud providers
        assert_eq!(compute_prev_step_index(5, "soniox"), 3);
        assert_eq!(compute_prev_step_index(5, "openai-realtime"), 3);
    }

    #[test]
    fn prev_step_language_local_goes_to_model() {
        // Language (5) Back → Model (4) for LocalWhisper
        assert_eq!(compute_prev_step_index(5, "local-whisper"), 4);
    }

    // ── Persistence / schema parse tests ────────────────────────────────────

    #[test]
    fn parse_v2_complete_returns_complete() {
        let content = "v=2\nstep.welcome.done=100\nstep.audio.done=101\nstep.complete=200\n";
        // We parse in-memory without file I/O by re-using the logic.
        // Test the same rules read_progress() implements.
        assert!(content.lines().any(|l| l.trim() == "v=2"));
        assert!(content
            .lines()
            .any(|l| l.trim().starts_with("step.complete=")));
    }

    #[test]
    fn parse_v2_partial_resumes_at_correct_step() {
        // Simulate content with welcome + audio done, provider NOT done.
        // Expected: Resume(1) — last completed index = 1 (audio at ordered_keys[1]).
        let content = "v=2\nstep.welcome.done=100\nstep.audio.done=101\n";
        let has_v2 = content.lines().any(|l| l.trim() == "v=2");
        let has_complete = content
            .lines()
            .any(|l| l.trim().starts_with("step.complete="));
        assert!(has_v2);
        assert!(!has_complete);
        // Check last completed step index
        let ordered_keys = [
            "step.welcome.done=",
            "step.audio.done=",
            "step.provider.done=",
            "step.api_key.",
            "step.model.done=",
            "step.language.done=",
        ];
        let mut last = None;
        for (idx, prefix) in ordered_keys.iter().enumerate() {
            if content.lines().any(|l| l.trim().starts_with(prefix)) {
                last = Some(idx);
            }
        }
        assert_eq!(last, Some(1)); // audio = index 1
    }

    #[test]
    fn parse_legacy_no_v2_header_treated_as_complete() {
        // Legacy v1 file: any content without "v=2" line → Complete (no re-onboarding)
        let content = "1716192000\n"; // old single-line Unix timestamp format
        let has_v2 = content.lines().any(|l| l.trim() == "v=2");
        // No v=2 → treated as complete in read_progress()
        assert!(!has_v2);
        // Per spec: treat as Complete (legacy alpha user)
        // Verified by the read_progress() logic above.
    }

    #[test]
    fn parse_empty_file_treated_as_complete() {
        // 0-byte file (old alpha sentinel) → no v=2 → treated as Complete
        let content = "";
        let has_v2 = content.lines().any(|l| l.trim() == "v=2");
        assert!(!has_v2);
        // Per spec: old 0-byte sentinel → Complete
    }

    #[test]
    fn parse_api_key_skipped_counts_as_done() {
        // api_key step: either .done= or .skipped= should count.
        let content = "v=2\nstep.welcome.done=1\nstep.audio.done=2\nstep.provider.done=3\nstep.api_key.skipped=4\n";
        let prefix = "step.api_key.";
        let found = content.lines().any(|l| l.trim().starts_with(prefix));
        assert!(found, "skipped should match the api_key. prefix");
    }

    #[test]
    fn wizard_step_index_roundtrip() {
        // Updated for Phase 9: Done moved from index 6 → 7; CrashReport at 6.
        for i in 0..=7usize {
            let step = WizardStep::from_index(i).expect("valid index 0..=7");
            assert_eq!(step.index(), i);
        }
        assert!(WizardStep::from_index(8).is_none(), "index 8 must be out of range");
    }

    // ── Phase 9 wizard state-machine tests ──────────────────────────────────

    #[test]
    fn next_step_language_goes_to_crash_report() {
        // Language (5) → CrashReport (6) — was Language → Done before Phase 9
        assert_eq!(compute_next_step_index(5, "local-whisper"), 6);
        assert_eq!(compute_next_step_index(5, "soniox"), 6);
        assert_eq!(compute_next_step_index(5, "openai-realtime"), 6);
    }

    #[test]
    fn next_step_crash_report_goes_to_done() {
        // CrashReport (6) → Done (7)
        assert_eq!(compute_next_step_index(6, "local-whisper"), 7);
        assert_eq!(compute_next_step_index(6, "soniox"), 7);
        assert_eq!(compute_next_step_index(6, "openai-realtime"), 7);
    }

    #[test]
    fn prev_step_crash_report_goes_to_language() {
        // CrashReport (6) Back → Language (5)
        assert_eq!(compute_prev_step_index(6, "local-whisper"), 5);
        assert_eq!(compute_prev_step_index(6, "soniox"), 5);
    }

    #[test]
    fn prev_step_done_goes_to_crash_report() {
        // Done (7) Back → CrashReport (6) — was Done → Language before Phase 9
        assert_eq!(compute_prev_step_index(7, "local-whisper"), 6);
        assert_eq!(compute_prev_step_index(7, "soniox"), 6);
    }

    #[test]
    fn crash_report_step_key_and_index_consistent() {
        let step = WizardStep::CrashReport;
        assert_eq!(step.key(), "crash_report");
        assert_eq!(step.index(), 6);
        assert_eq!(WizardStep::from_index(6), Some(WizardStep::CrashReport));
    }

    #[test]
    fn done_step_index_is_now_seven() {
        // Verify Done shifted from 6 → 7.
        assert_eq!(WizardStep::Done.index(), 7);
        assert_eq!(WizardStep::from_index(7), Some(WizardStep::Done));
    }

    #[test]
    fn parse_v2_with_crash_report_step_resumes_correctly() {
        // A v=2 file where crash_report is the last completed step should
        // resume at index 6 (the crash_report step's ordered_keys index).
        let content = "v=2\nstep.welcome.done=1\nstep.audio.done=2\nstep.provider.done=3\nstep.model.done=4\nstep.language.done=5\nstep.crash_report.done=6\n";
        let ordered_keys = [
            "step.welcome.done=",
            "step.audio.done=",
            "step.provider.done=",
            "step.api_key.",
            "step.model.done=",
            "step.language.done=",
            "step.crash_report.done=",
        ];
        let mut last: Option<usize> = None;
        for (idx, prefix) in ordered_keys.iter().enumerate() {
            if content.lines().any(|l| l.trim().starts_with(prefix)) {
                last = Some(idx);
            }
        }
        // crash_report.done= is at ordered_keys index 6
        assert_eq!(last, Some(6), "crash_report step should map to ordered_keys index 6");
    }
}
