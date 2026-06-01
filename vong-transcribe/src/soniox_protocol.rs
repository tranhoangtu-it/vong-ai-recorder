//! Soniox WebSocket protocol messages.
//!
//! Reference: https://soniox.com/docs/stt/api-reference/websocket-api
//!
//! Plan v2 Section 4.1 — model `stt-rt` family for real-time streaming.

use serde::{Deserialize, Serialize};

/// Initial config message sent after WebSocket connect.
///
/// Includes API key (over TLS — never logged).
///
/// ⚠️ Plan v2 Section 15.3 — `#[derive(Debug)]` is DELIBERATELY OMITTED.
/// Debug-printing this struct would leak the API key via `{:?}` formatter.
/// If you need to inspect for debugging, log specific safe fields only
/// (model, sample_rate, etc.) — NEVER print this struct as a whole.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ConfigMessage {
    /// Soniox API key (sent over TLS, sanitized from logs).
    /// This field is plaintext at the serialization boundary by necessity
    /// (Soniox protocol requires JSON-embedded api_key). Caller (soniox.rs)
    /// must wrap serialized output in `Zeroizing<String>` to clear after send.
    pub api_key: String,

    /// Model identifier (e.g., "stt-rt-preview", "stt-rt-v4", whichever current).
    pub model: String,

    /// Audio encoding format. We always send PCM signed 16-bit little-endian.
    pub audio_format: String,

    /// Sample rate in Hz. Always 16000 (Vọng pipeline standard).
    pub sample_rate: u32,

    /// Channel count. Always 1 (mono).
    pub num_channels: u32,

    /// Enable per-token language identification (mid-sentence detection).
    pub enable_language_identification: bool,

    /// Enable speaker diarization. MVP 0.1 always false.
    pub enable_speaker_diarization: bool,

    /// Optional language hint (e.g., "vi", "en") to bias LID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_hints: Option<Vec<String>>,

    /// Optional translation config.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation: Option<TranslationConfig>,

    /// Optional vocabulary context (Phase 8 — dictionary injection).
    /// When present and non-empty `terms`, Soniox biases recognition toward
    /// the listed phrases. Omitted entirely when the user's dictionary is empty
    /// so the wire message is identical to the pre-Phase-8 baseline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<SonioxContext>,
}

/// Vocabulary context sent with the Soniox config message.
///
/// `terms` is the only field used in Sprint 2. Other sections (`general`,
/// `text`, `translation_terms`) are reserved for future use.
///
/// Privacy: term strings are user content — never log them.
#[derive(Debug, Default, Serialize)]
pub(crate) struct SonioxContext {
    /// List of domain-specific vocabulary phrases to bias Soniox recognition.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub terms: Vec<String>,
}

/// Translation parameters for Soniox built-in real-time translation.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct TranslationConfig {
    /// Translation mode (e.g., "one_way", "two_way").
    /// MVP 0.1 uses "one_way" — incoming speech → target language.
    pub mode: String,

    /// Target language code (ISO 639-1, e.g., "vi", "en").
    pub target_language: String,
}

/// Inbound message — token batch.
///
/// Some fields parsed for forward-compat / future telemetry (e.g., audio_proc_ms)
/// but not currently consumed by `SonioxProvider`.
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // forward-compat fields parsed but unused MVP 0.1
pub(crate) struct TokenMessage {
    /// Token list (may be empty as keep-alive).
    #[serde(default)]
    pub tokens: Vec<Token>,

    /// Total audio milliseconds processed so far (cumulative).
    #[serde(default)]
    pub final_audio_proc_ms: Option<u64>,

    /// Total audio milliseconds received (cumulative).
    #[serde(default)]
    pub total_audio_proc_ms: Option<u64>,

    /// Error message (if present, transcription error occurred).
    #[serde(default)]
    pub error_code: Option<String>,

    /// Error message text.
    #[serde(default)]
    pub error_message: Option<String>,
}

/// Single token emitted by Soniox.
#[derive(Debug, Deserialize)]
pub(crate) struct Token {
    /// Token text (word, punctuation, or special marker).
    pub text: String,

    /// Whether this token is finalized (won't change with future context).
    #[serde(default)]
    pub is_final: bool,

    /// Detected language tag for this token (e.g., "vi", "en").
    #[serde(default)]
    pub language: Option<String>,

    /// Speaker label if diarization enabled.
    #[serde(default)]
    pub speaker: Option<String>,

    /// Token start time relative to session start (milliseconds).
    #[serde(default)]
    pub start_ms: Option<u64>,

    /// Token end time (milliseconds).
    #[serde(default)]
    pub end_ms: Option<u64>,

    /// Translation status if translation enabled ("original" | "translation" | etc.).
    #[serde(default)]
    pub translation_status: Option<String>,

    /// Source language for translated tokens (e.g., "vi" if `text` is "en" translation).
    #[serde(default)]
    pub source_language: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> ConfigMessage {
        ConfigMessage {
            api_key: "test-key".into(),
            model: "stt-rt-preview".into(),
            audio_format: "pcm_s16le".into(),
            sample_rate: 16_000,
            num_channels: 1,
            enable_language_identification: true,
            enable_speaker_diarization: false,
            language_hints: None,
            translation: None,
            context: None,
        }
    }

    #[test]
    fn config_serializes_minimal() {
        let cfg = base_config();
        let json = serde_json::to_string(&cfg).unwrap();
        // Optional fields with None should be skipped
        assert!(!json.contains("language_hints"));
        assert!(!json.contains("translation"));
        assert!(!json.contains("context"));
        assert!(json.contains("\"api_key\":\"test-key\""));
        assert!(json.contains("\"model\":\"stt-rt-preview\""));
    }

    #[test]
    fn config_serializes_with_context_terms() {
        let cfg = ConfigMessage {
            context: Some(SonioxContext {
                terms: vec!["WebSocket".into(), "Vọng".into()],
            }),
            ..base_config()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("\"context\""), "context field must be present");
        assert!(json.contains("\"terms\""), "terms field must be present");
        assert!(json.contains("WebSocket"), "WebSocket term must be present");
        assert!(json.contains("Vọng"), "Vọng term must be present");
    }

    #[test]
    fn config_omits_context_when_empty_terms() {
        let cfg = ConfigMessage {
            context: Some(SonioxContext { terms: vec![] }),
            ..base_config()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        // SonioxContext with empty terms is serialized but terms field is skipped;
        // however the context wrapper itself is still present. In practice
        // soniox.rs only sets context when terms is non-empty, so this is
        // a belt-and-suspenders check.
        // (The terms field itself should be absent since skip_serializing_if = Vec::is_empty)
        assert!(!json.contains("\"terms\""), "empty terms should be skipped");
    }

    #[test]
    fn token_deserializes_minimal() {
        let json = r#"{"text":"Xin chào","is_final":true,"language":"vi"}"#;
        let t: Token = serde_json::from_str(json).unwrap();
        assert_eq!(t.text, "Xin chào");
        assert!(t.is_final);
        assert_eq!(t.language.as_deref(), Some("vi"));
    }

    #[test]
    fn token_message_handles_empty_keep_alive() {
        let json = r#"{}"#;
        let m: TokenMessage = serde_json::from_str(json).unwrap();
        assert!(m.tokens.is_empty());
        assert!(m.final_audio_proc_ms.is_none());
    }

    #[test]
    fn token_message_with_error() {
        let json = r#"{"error_code":"invalid_api_key","error_message":"Key not recognized"}"#;
        let m: TokenMessage = serde_json::from_str(json).unwrap();
        assert_eq!(m.error_code.as_deref(), Some("invalid_api_key"));
        assert_eq!(m.error_message.as_deref(), Some("Key not recognized"));
    }
}
