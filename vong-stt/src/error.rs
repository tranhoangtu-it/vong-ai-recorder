//! STT subsystem error types.

use thiserror::Error;

/// Errors emitted by STT providers and supporting infrastructure.
#[derive(Debug, Error)]
pub enum SttError {
    /// Failure interacting with OS Keychain or credential store.
    #[error("credential store error: {0}")]
    Credential(String),

    /// Network / transport error (DNS, TLS, WebSocket).
    #[error("network error: {0}")]
    Network(String),

    /// Provider API returned an error response.
    #[error("provider error: code={code} message={message}")]
    Provider {
        /// Provider-specific error code (e.g., "invalid_api_key", "quota_exceeded").
        code: String,
        /// Human-readable message (sanitized — no PII).
        message: String,
    },

    /// JSON deserialization failed on incoming token message.
    #[error("protocol decode error: {0}")]
    ProtocolDecode(String),

    /// WebSocket reconnect attempts exhausted.
    #[error("reconnect exhausted after {attempts} attempts")]
    ReconnectExhausted {
        /// Number of attempts before giving up.
        attempts: u32,
    },

    /// Downstream consumer (e.g., UI event subscriber) closed.
    #[error("downstream event channel closed")]
    DownstreamClosed,

    /// Configuration error (e.g., missing required field).
    #[error("config error: {0}")]
    Config(String),
}

impl From<serde_json::Error> for SttError {
    fn from(e: serde_json::Error) -> Self {
        Self::ProtocolDecode(e.to_string())
    }
}
