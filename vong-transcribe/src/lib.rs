//! Vọng AI Recorder — transcription engines.
//!
//! Three providers behind a single `StreamingTranscriber` trait:
//! `WhisperLocalProvider` (offline, CPU or Vulkan), `SonioxProvider`
//! (BYOK WebSocket, true word-level partials), `OpenAIRealtimeProvider`
//! (BYOK WebSocket, `gpt-4o-mini-transcribe` by default).
//!
//! # Quick start
//!
//! ```no_run
//! use vong_transcribe::{ApiKey, SonioxProvider, StreamOpts, StreamingTranscriber};
//! use tokio::sync::mpsc;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Load API key from OS Keychain
//! let key = ApiKey::load("soniox")?;
//! let provider = SonioxProvider::new(key);
//!
//! // Channels: audio in (from VAD), events out (to UI + storage)
//! let (audio_tx, audio_rx) = mpsc::channel(32);
//! let (event_tx, mut event_rx) = mpsc::channel(256);
//!
//! let opts = StreamOpts {
//!     language_hint: Some("vi".into()),
//!     enable_lid: true,
//!     enable_diarization: false,
//!     enable_translation_to: None,
//! };
//!
//! tokio::spawn(async move {
//!     let _ = provider.transcribe_stream(audio_rx, event_tx, opts).await;
//! });
//!
//! while let Some(event) = event_rx.recv().await {
//!     // Forward to floating pill UI + SQLite history
//!     println!("event: {:?}", event);
//! }
//! # let _ = audio_tx;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]

mod api_key;
mod error;
mod events;
pub mod model_dl;
mod openai_realtime;
mod reconnect;
mod soniox;
mod soniox_protocol;
pub mod summary;
mod traits;
mod whisper_local;

pub use api_key::ApiKey;
pub use error::SttError;
pub use events::TranscriptEvent;
pub use model_dl::{
    cleanup_stale_parts, download_model, find_spec, parse_lfs_pointer, DlError, DownloadProgress,
    DownloadState, ModelSpec, MODELS,
};
pub use openai_realtime::OpenAIRealtimeProvider;
pub use reconnect::ExponentialBackoff;
pub use soniox::SonioxProvider;
pub use traits::{
    build_openai_instructions, build_soniox_terms, build_whisper_prompt, DictContext, DictEntry,
    LiveConfigHandle, LiveSttConfig, ProviderMode, StreamOpts, StreamingTranscriber, TargetMode,
    MAX_DICTIONARY_ENTRIES, PHRASE_MAX_BYTES,
};
pub use whisper_local::{WhisperLocalProvider, DEFAULT_MODEL_FILENAME};
pub use summary::{generate_summary, truncate_transcript, SummaryError, SummaryResponse};

/// Crate version.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
