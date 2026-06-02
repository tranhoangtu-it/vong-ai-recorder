//! Voice Typing — global hotkey triggered dictation with text injection.
//!
//! # Flow
//! Hotkey (default Ctrl+Shift+V) → `Idle → Listening` (short-lived mic capture)
//! → VAD silence or max-duration → `Transcribing` (Whisper Pass A via transcribe_stream)
//! → `Injecting` (enigo.text()) → `Idle`.
//!
//! Cancellation: pressing the hotkey again while Listening cancels.
//!
//! # Windows only
//! Text injection is gated on `#[cfg(target_os = "windows")]`. On other
//! platforms the inject path is a no-op that returns an error variant; the
//! UI toggle is still rendered but is disabled with a platform note.
//!
//! # Privacy
//! - State transitions and character counts are safe to log.
//! - NEVER log the transcribed text — it is user content.
//! - Injected text passes through `sanitize_for_injection` which strips NUL,
//!   ESC, and BS control characters before sending to enigo.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::mpsc;
use uuid::Uuid;

use vong_audio::{
    default_input_device, negotiate_config, run_resampler, start_capture, DeviceKind,
    OverflowCounter, PeakMeter, ResampleConfig, Utterance,
};
use vong_transcribe::{StreamOpts, StreamingTranscriber, TranscriptEvent, WhisperLocalProvider};

use crate::recording_config::VoiceTypingConfig;
use crate::toast::{ToastQueue, ToastSeverity};

// ──────────────────────────────────────────────────────────────────────────────
// State machine
// ──────────────────────────────────────────────────────────────────────────────

/// Voice Typing session states. Transitions are strictly ordered.
///
/// ```text
/// Idle → Listening → Transcribing → Injecting → Idle
///          ↓ (cancel hotkey or main pipeline recording active)
///         Idle
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Error variant reserved for future explicit error display
pub enum VoiceTypingState {
    /// Hotkey armed but no session in progress.
    Idle,
    /// Mic is open, VAD collecting audio. `started_at` tracks elapsed time for overlay.
    Listening { started_at: Instant },
    /// Whisper inference running on the collected audio.
    Transcribing,
    /// enigo sending the injected text to the focused app.
    Injecting,
    /// Terminal error — cleared to Idle after the toast is shown.
    Error(String),
}

impl VoiceTypingState {
    /// Stable string token sent to Slint UI overlay.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Listening { .. } => "listening",
            Self::Transcribing => "transcribing",
            Self::Injecting => "injecting",
            Self::Error(_) => "error",
        }
    }

    /// True when a session is in progress (not `Idle` or `Error`).
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Listening { .. } | Self::Transcribing | Self::Injecting)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Shared handle
// ──────────────────────────────────────────────────────────────────────────────

/// Shared, thread-safe handle to the current Voice Typing state.
pub type VoiceTypingHandle = Arc<Mutex<VoiceTypingState>>;

/// Create a new handle in the `Idle` state.
pub fn new_handle() -> VoiceTypingHandle {
    Arc::new(Mutex::new(VoiceTypingState::Idle))
}

// ──────────────────────────────────────────────────────────────────────────────
// Text sanitizer
// ──────────────────────────────────────────────────────────────────────────────

/// Strip control characters that must never reach `enigo.text()`.
///
/// Allowed: printable Unicode (CJK, diacritics, emoji), `\n`, `\t`, space.
/// Stripped: `\0` (NUL), `\x1b` (ESC), `\x08` (BS), all other C0/C1 controls.
pub fn sanitize_for_injection(input: &str) -> String {
    input
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect()
}

// ──────────────────────────────────────────────────────────────────────────────
// Injection
// ──────────────────────────────────────────────────────────────────────────────

/// Inject `text` into the currently focused application via OS synthetic input.
///
/// On Windows uses `enigo` (SendInput). On other platforms returns an error —
/// the caller shows a toast. Text is sanitized before sending.
pub fn inject_text(text: &str) -> Result<(), String> {
    let sanitized = sanitize_for_injection(text);
    if sanitized.is_empty() {
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        use enigo::{Enigo, Keyboard, Settings};
        let mut enigo = Enigo::new(&Settings::default())
            .map_err(|e| format!("enigo init: {e}"))?;
        enigo.text(&sanitized).map_err(|e| format!("enigo.text: {e}"))?;
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = sanitized;
        Err("Text injection chỉ hỗ trợ Windows".to_string())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Session error type
// ──────────────────────────────────────────────────────────────────────────────

/// Errors that can occur during a voice typing session.
#[derive(Debug, thiserror::Error)]
pub enum VoiceTypingError {
    #[error("Không truy cập được mic: {0}")]
    MicUnavailable(String),
    #[error("Mô hình Whisper chưa cài")]
    WhisperModelMissing,
    #[error("Không gõ được — focus app khác và thử lại: {0}")]
    InjectionFailed(String),
    #[error("Đang ghi phiên chính — dừng phiên trước")]
    MainPipelineActive,
    #[error("{0}")]
    Other(String),
}

impl VoiceTypingError {
    pub fn to_toast_message(&self) -> &str {
        match self {
            Self::MicUnavailable(_) => "Không truy cập được mic",
            Self::WhisperModelMissing => "Mô hình Whisper chưa cài",
            Self::InjectionFailed(_) => "Không gõ được — focus app khác và thử lại",
            Self::MainPipelineActive => "Đang ghi phiên chính — dừng phiên trước",
            Self::Other(m) => m,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Top-level session dispatcher
// ──────────────────────────────────────────────────────────────────────────────

/// Run one complete voice-typing session (must be called from a tokio task).
///
/// Handles all state transitions, error toasting, and injection.
/// Always returns to `Idle` state regardless of outcome.
pub async fn run_session(
    config: VoiceTypingConfig,
    handle: VoiceTypingHandle,
    is_main_recording: bool,
    model_path: std::path::PathBuf,
    toast: ToastQueue,
) {
    if is_main_recording {
        toast.push(
            VoiceTypingError::MainPipelineActive.to_toast_message(),
            ToastSeverity::Warn,
        );
        tracing::info!("voice_typing: aborted — main pipeline is recording");
        return;
    }

    // Transition → Listening.
    set_state(&handle, VoiceTypingState::Listening { started_at: Instant::now() });
    tracing::info!(
        max_duration_secs = config.max_duration_secs,
        language = %config.language,
        "voice_typing: session started"
    );

    let result = run_session_inner(&config, model_path, handle.clone()).await;

    match result {
        Ok(Some(text)) => {
            set_state(&handle, VoiceTypingState::Injecting);
            tracing::info!(char_count = text.chars().count(), "voice_typing: injecting");
            match inject_text(&text) {
                Ok(()) => {
                    tracing::info!(
                        char_count = text.chars().count(),
                        "voice_typing: text injected successfully"
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, "voice_typing: injection failed");
                    toast.push(
                        VoiceTypingError::InjectionFailed(e).to_toast_message(),
                        ToastSeverity::Error,
                    );
                }
            }
        }
        Ok(None) => {
            tracing::info!("voice_typing: session produced no text (cancelled or empty)");
        }
        Err(e) => {
            tracing::warn!(error = %e, "voice_typing: session error");
            toast.push(e.to_toast_message(), ToastSeverity::Error);
        }
    }

    // Always return to Idle.
    set_state(&handle, VoiceTypingState::Idle);
    tracing::info!("voice_typing: session complete → Idle");
}

/// Inner session: captures audio, builds Utterance, runs Whisper via
/// `transcribe_stream` (single-utterance one-shot), returns transcribed text.
/// Returns `Ok(None)` on cancellation or empty result.
async fn run_session_inner(
    config: &VoiceTypingConfig,
    model_path: std::path::PathBuf,
    handle: VoiceTypingHandle,
) -> Result<Option<String>, VoiceTypingError> {
    // ── 1. Load Whisper model ──────────────────────────────────────────────
    let provider = WhisperLocalProvider::new(&model_path)
        .map_err(|_| VoiceTypingError::WhisperModelMissing)?;

    // Seed the provider's live config with the voice-typing language.
    {
        if let Ok(mut cfg) = provider.live_config().lock() {
            cfg.source_language = Some(config.language.clone());
            // Pass B (translation) is not needed for voice typing — fast path only.
            cfg.target_mode = vong_transcribe::TargetMode::Off;
        }
    }

    // ── 2. Open mic (default input device) ────────────────────────────────
    let peak = PeakMeter::new();
    let overflow = OverflowCounter::new();

    // Ring buffer: allow up to max_duration + 2 s of headroom at 16 kHz mono.
    let ring_size = 16_000usize * 2 * (config.max_duration_secs as usize + 2);
    let (ring_tx, ring_rx) = rtrb::RingBuffer::<i16>::new(ring_size);

    let device = default_input_device()
        .map_err(|e| VoiceTypingError::MicUnavailable(e.to_string()))?;

    let supported = negotiate_config(&device, DeviceKind::Input)
        .map_err(|e| VoiceTypingError::MicUnavailable(e.to_string()))?;

    let sample_rate = supported.sample_rate();
    let channels = supported.channels();

    let _capture = start_capture(&device, DeviceKind::Input, ring_tx, peak, overflow)
        .map_err(|e| VoiceTypingError::MicUnavailable(e.to_string()))?;

    // ── 3. Resample to 16 kHz mono ─────────────────────────────────────────
    let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<i16>>(64);
    let resample_cfg = ResampleConfig {
        source_rate: sample_rate,
        source_channels: channels.max(1),
        read_chunk_size: 2048,
    };
    let resampler_handle = tokio::spawn(async move {
        if let Err(e) = run_resampler(ring_rx, audio_tx, resample_cfg).await {
            tracing::debug!(error = ?e, "voice_typing resampler exited");
        }
    });

    // ── 4. Collect audio until silence or max-duration ─────────────────────
    let max_duration = Duration::from_secs(config.max_duration_secs as u64);

    // Simple energy-based silence detection.
    // VAD on main pipeline is too heavyweight to reuse here — keep it simple.
    const SILENCE_RMS_THRESHOLD: f32 = 0.002;
    const SILENCE_GATE: Duration = Duration::from_millis(1500);
    const MIN_SPEECH: Duration = Duration::from_millis(300);

    let session_start = Instant::now();
    let mut collected: Vec<i16> = Vec::new();
    let mut had_speech = false;
    let mut silence_since: Option<Instant> = None;

    'collect: loop {
        // Cancellation check: if state was externally reset to Idle (second hotkey press).
        {
            let g = handle.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(*g, VoiceTypingState::Idle) {
                resampler_handle.abort();
                return Ok(None);
            }
        }

        if session_start.elapsed() >= max_duration {
            tracing::info!(
                duration_ms = session_start.elapsed().as_millis(),
                "voice_typing: max duration reached — packing"
            );
            break 'collect;
        }

        let chunk_opt = tokio::time::timeout(Duration::from_millis(30), audio_rx.recv()).await;
        match chunk_opt {
            Ok(Some(chunk)) => {
                let rms = compute_rms(&chunk);
                collected.extend_from_slice(&chunk);

                if rms > SILENCE_RMS_THRESHOLD {
                    had_speech = true;
                    silence_since = None;
                } else if had_speech && silence_since.is_none() {
                    silence_since = Some(Instant::now());
                }

                // Break when speech was detected and silence gate has elapsed.
                if had_speech
                    && session_start.elapsed() >= MIN_SPEECH
                    && silence_since.map(|t| t.elapsed() >= SILENCE_GATE).unwrap_or(false)
                {
                    tracing::info!(
                        duration_ms = session_start.elapsed().as_millis(),
                        "voice_typing: silence gate reached — packing"
                    );
                    break 'collect;
                }
            }
            Ok(None) => break 'collect,
            Err(_) => {} // timeout — loop again
        }
    }

    resampler_handle.abort();

    if collected.is_empty() {
        return Ok(None);
    }

    // ── 5. Build a single final Utterance ─────────────────────────────────
    let duration_ms = (collected.len() as u32) / 16; // 16 samples/ms @ 16 kHz
    let utterance = Utterance {
        id: Uuid::new_v4(),
        seq: 0,
        started_at: SystemTime::now(),
        duration_ms,
        audio_pcm16_mono_16k: collected,
        is_partial: false,
    };

    // ── 6. Transition → Transcribing ──────────────────────────────────────
    set_state(&handle, VoiceTypingState::Transcribing);

    // ── 7. Run Whisper via transcribe_stream (one-shot channel) ──────────
    let (utt_tx, utt_rx) = mpsc::channel::<Utterance>(1);
    let (event_tx, mut event_rx) = mpsc::channel::<TranscriptEvent>(8);

    // Send the utterance then drop the sender so the stream ends after one utt.
    utt_tx
        .send(utterance)
        .await
        .map_err(|_| VoiceTypingError::Other("utt channel send failed".into()))?;
    drop(utt_tx);

    let opts = StreamOpts {
        language_hint: Some(config.language.clone()),
        enable_lid: false,
        enable_diarization: false,
        enable_translation_to: None,
    };

    // Drive the stream on a tokio task so we can concurrently drain the event channel.
    let stream_handle = tokio::spawn(async move {
        if let Err(e) = provider.transcribe_stream(utt_rx, event_tx, opts).await {
            tracing::debug!(error = ?e, "voice_typing: transcribe_stream ended");
        }
    });

    // Collect the Final event text.
    let mut result_text = String::new();
    while let Some(event) = event_rx.recv().await {
        match event {
            TranscriptEvent::Final { original_text, text, .. } => {
                // Pass A result is in `original_text` when TargetMode is Off.
                // `original_text` = auto-detect pass (Pass A). Use that for voice typing.
                let candidate = if !original_text.is_empty() {
                    original_text
                } else {
                    text
                };
                if !candidate.is_empty() {
                    result_text = candidate;
                }
            }
            TranscriptEvent::Disconnected => break,
            TranscriptEvent::Error { code, message } => {
                tracing::warn!(code = %code, message = %message, "voice_typing: STT error");
                break;
            }
            _ => {}
        }
    }

    // Wait for the stream task to finish (it should have exited by now).
    let _ = tokio::time::timeout(Duration::from_secs(5), stream_handle).await;

    let trimmed = result_text.trim().to_string();
    if trimmed.is_empty() {
        return Ok(None);
    }

    Ok(Some(trimmed))
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

fn set_state(handle: &VoiceTypingHandle, state: VoiceTypingState) {
    let mut g = handle.lock().unwrap_or_else(|e| e.into_inner());
    *g = state;
}

fn compute_rms(chunk: &[i16]) -> f32 {
    if chunk.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = chunk.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum();
    (sum_sq / chunk.len() as f64).sqrt() as f32
}
