//! Vọng Speech-to-Text providers.
//!
//! Phase 4: Soniox WebSocket integration (BYOK, real-time translation).
//! Future: OpenAI gpt-realtime, Google Chirp 3, Whisper Local.

#![warn(missing_docs)]
#![warn(clippy::all)]

/// Phase 0 placeholder. Will export `StreamingTranscriber` trait + `SonioxProvider` Phase 4.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
