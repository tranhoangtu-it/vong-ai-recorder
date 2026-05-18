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
    ///
    /// Carries two parallel renditions of the same utterance:
    /// - `text` / `language` — produced with the user-selected language hint
    ///   (e.g., `vi`). This is the "bản phiên âm" surfaced as the primary stream.
    /// - `original_text` / `original_language` — produced with no language hint,
    ///   letting Whisper auto-detect. This is the "bản gốc" — preserves the
    ///   source language when the speaker is using something other than Vietnamese.
    ///
    /// Providers that don't run a second pass (e.g., Soniox) may leave the
    /// `original_*` fields equal to the primary ones, or `None`/empty for
    /// `original_language`.
    Final {
        /// Sequential token index.
        seq: u64,
        /// Primary token text (language-hinted pass).
        text: String,
        /// Hinted/detected language tag for `text`.
        language: Option<String>,
        /// Original-language text (auto-detect pass). Empty if not produced.
        original_text: String,
        /// Auto-detected language tag for `original_text`. `None` if unavailable.
        original_language: Option<String>,
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
