//! STT transcript events — partial/final tokens, translation, status.
//!
//! Plan v2 Section 4.3 — `TranscriptEvent` enum is the contract between
//! STT providers and consumers (UI floating pill, SQLite history writer).

use std::time::Duration;

/// Single event emitted by a streaming STT provider.
///
/// Consumers should expect:
/// - `Connected` once per session start
/// - 0..N `Partial` events as tokens arrive non-final
/// - 1..N `Final` events as tokens commit (and may overwrite Partial text)
/// - 0..N `Translation` events if `enable_translation_to` requested
/// - `Disconnected` on graceful close
/// - `Error` for recoverable provider errors
#[derive(Debug, Clone)]
pub enum TranscriptEvent {
    /// WebSocket connection established + config message accepted.
    Connected,

    /// Non-final token — text may change as more context arrives.
    Partial {
        /// Sequential token index within the session.
        seq: u64,
        /// Token text (sanitized — caller should NFC-normalize before display).
        text: String,
        /// Detected language tag (ISO 639-1, e.g., "vi", "en"). None if LID disabled.
        language: Option<String>,
    },

    /// Final committed token — text won't change for this segment.
    Final {
        /// Sequential token index.
        seq: u64,
        /// Token text.
        text: String,
        /// Detected language tag.
        language: Option<String>,
        /// Speaker label if diarization enabled (e.g., "spk-1"). MVP 0.1: always None.
        speaker: Option<String>,
        /// Segment start time relative to session start.
        start: Duration,
        /// Segment end time.
        end: Duration,
    },

    /// Translation output if `enable_translation_to` was set.
    Translation {
        /// Sequential token index (matches source Final.seq).
        seq: u64,
        /// Translated text.
        text: String,
        /// Source language code.
        source_lang: String,
        /// Target language code (matches StreamOpts.enable_translation_to).
        target_lang: String,
        /// Whether this translation is final or may update.
        is_final: bool,
    },

    /// Provider returned a recoverable error (e.g., temporary rate limit).
    /// Fatal errors return via `Err(SttError)` instead.
    Error {
        /// Provider-specific code (e.g., "rate_limited").
        code: String,
        /// Human message (no PII).
        message: String,
    },

    /// WebSocket closed (graceful). Reconnect logic may follow.
    Disconnected,
}
