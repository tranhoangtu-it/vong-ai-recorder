//! STT provider trait abstraction.
//!
//! Plan v2 Section 4.3 — Strategy pattern dual signature (batch + streaming).
//! MVP 0.1 ships only `StreamingTranscriber` (Soniox). Batch trait deferred
//! to MVP 0.2 (OpenAI Whisper-1 REST + AssemblyAI batch).

use crate::error::SttError;
use crate::events::TranscriptEvent;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
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

/// Which STT engine drives the pipeline.
///
/// Selected at app startup from the persisted user config; changing it in the
/// UI updates the config but requires a restart to take effect (hot-swap of
/// the running stream is not supported yet — would need to tear down the
/// active provider task and re-spawn).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderMode {
    /// Local Whisper.cpp via `WhisperLocalProvider`. Free, runs on GPU/CPU,
    /// offline. Batches per utterance — best partial granularity ~1.5 s.
    LocalWhisper,
    /// Soniox WebSocket via `SonioxProvider`. BYOK ($0.003/min). True
    /// word-level streaming partials, language ID, optional translation.
    SonioxCloud,
    /// OpenAI gpt-realtime via the Realtime WebSocket API. BYOK.
    /// Provider implementation pending — selecting this returns an error
    /// at startup until the provider lands.
    OpenAIRealtime,
}

impl Default for ProviderMode {
    fn default() -> Self {
        Self::LocalWhisper
    }
}

impl ProviderMode {
    /// Stable string id used by config persistence + UI option labels.
    pub fn id(&self) -> &'static str {
        match self {
            Self::LocalWhisper => "local-whisper",
            Self::SonioxCloud => "soniox",
            Self::OpenAIRealtime => "openai-realtime",
        }
    }

    /// Parse from the stable id (returns `None` on unknown).
    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "local-whisper" => Some(Self::LocalWhisper),
            "soniox" => Some(Self::SonioxCloud),
            "openai-realtime" => Some(Self::OpenAIRealtime),
            _ => None,
        }
    }
}

/// What the second Whisper pass (the "Bản dịch" column) should produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetMode {
    /// Don't run pass B. UI just shows "Bản gốc" populated; "Bản dịch" stays
    /// in `translation_done=true` empty state ("(không có)").
    Off,
    /// Run Whisper with `set_translate(true)` — the model's only true
    /// cross-lingual translation, target language is always English.
    /// Use this when the user wants English subtitles for any source.
    TranslateToEnglish,
    /// Run Whisper with `set_language(Some(code))` and `set_translate(false)`.
    /// Real transcription of the source forced into the named language —
    /// gives correct text only when the input is actually that language;
    /// produces phonetic transliteration garbage on cross-lingual input.
    /// Useful when user wants a denoised/normalized version of the source
    /// (e.g., source is Vietnamese → target also Vietnamese for cleanup).
    Hint(String),
}

impl Default for TargetMode {
    fn default() -> Self {
        // Default — produces real English translation regardless of input
        // language. Pre-fix default (Hint("vi")) was the source of the
        // "khá tệ hại" complaint when speaker used English.
        Self::TranslateToEnglish
    }
}

/// Live STT configuration that can change between utterances.
///
/// Held behind `Arc<Mutex<...>>` and read by the streaming provider on every
/// utterance — the UI can mutate this without restarting the stream.
#[derive(Debug, Clone, Default)]
pub struct LiveSttConfig {
    /// Source language for "Bản gốc" (pass A). `None` = Whisper auto-detect.
    /// When `Some("en")` the model is told upfront → faster + more accurate
    /// than relying on the auto-detect head, at the cost of producing
    /// gibberish if the input isn't actually that language.
    pub source_language: Option<String>,

    /// Behavior for "Bản dịch" (pass B). See `TargetMode` doc.
    pub target_mode: TargetMode,
}

/// Convenience: handle to the shared live config.
pub type LiveConfigHandle = Arc<Mutex<LiveSttConfig>>;

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
