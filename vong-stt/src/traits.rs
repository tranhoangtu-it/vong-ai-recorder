//! STT provider trait abstraction.
//!
//! Plan v2 Section 4.3 — Strategy pattern dual signature (batch + streaming).
//! MVP 0.1 ships only `StreamingTranscriber` (Soniox). Batch trait deferred
//! to MVP 0.2 (OpenAI Whisper-1 REST + AssemblyAI batch).

use crate::error::SttError;
use crate::events::TranscriptEvent;
use async_trait::async_trait;
use tokio::sync::mpsc;
use vong_audio::Utterance;

/// Per-stream configuration options.
#[derive(Debug, Clone, Default)]
pub struct StreamOpts {
    /// Language hint (ISO 639-1, e.g., "vi"). `None` = auto LID.
    pub language_hint: Option<String>,

    /// Enable mid-sentence language identification (Soniox feature).
    pub enable_lid: bool,

    /// Enable speaker diarization. MVP 0.1: defer (false).
    pub enable_diarization: bool,

    /// Translate to this language code (e.g., "vi", "en"). `None` = transcript only.
    pub enable_translation_to: Option<String>,
}

/// Streaming STT provider trait.
///
/// Consumes utterances (full audio chunks bounded by VAD), emits transcript
/// events (partial + final tokens + translations) via mpsc.
///
/// Trait object compatible (uses `async_trait` macro) for runtime provider swap.
#[async_trait]
pub trait StreamingTranscriber: Send + Sync {
    /// Run the streaming transcription loop.
    ///
    /// Consumes utterances from `audio_rx` until the channel is closed,
    /// streams resulting events to `event_tx`. Blocks for the lifetime of
    /// the stream (suitable for `tokio::spawn`).
    ///
    /// # Errors
    /// - `SttError::ReconnectExhausted` if WebSocket fails persistently.
    /// - `SttError::DownstreamClosed` if `event_tx` consumer dropped.
    /// - `SttError::Provider`/`Network` for transient runtime issues.
    async fn transcribe_stream(
        &self,
        audio_rx: mpsc::Receiver<Utterance>,
        event_tx: mpsc::Sender<TranscriptEvent>,
        opts: StreamOpts,
    ) -> Result<(), SttError>;

    /// Stable provider identifier (e.g., "soniox", "openai-realtime", "google-chirp3").
    fn name(&self) -> &'static str;
}
