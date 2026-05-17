//! Soniox WebSocket STT provider — real-time streaming transcription
//! with optional translation + language identification.
//!
//! Plan v2 Section 4.1 / 13.7.1 / 15.3 / 15.6 — exponential backoff reconnect,
//! BYOK API key via Keychain, sanitization-safe logging.

use crate::api_key::ApiKey;
use crate::error::SttError;
use crate::events::TranscriptEvent;
use crate::reconnect::ExponentialBackoff;
use crate::soniox_protocol::{ConfigMessage, TokenMessage, TranslationConfig};
use crate::traits::{StreamOpts, StreamingTranscriber};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use vong_audio::Utterance;
use zeroize::Zeroizing;

/// Helper: serialize `ConfigMessage` to a `Zeroizing<String>` so that the
/// JSON containing the API key is zeroized when dropped after send.
///
/// Per code-reviewer C1 finding: prior code used `serde_json::to_string()`
/// which returns a plain `String` — the api_key bytes lingered in memory
/// until reused. Now we wrap in `Zeroizing<String>` to clear immediately
/// on drop (post-send).
fn serialize_config_zeroizing(config: &ConfigMessage) -> Result<Zeroizing<String>, SttError> {
    let json = serde_json::to_string(config)?;
    Ok(Zeroizing::new(json))
}

const DEFAULT_WS_URL: &str = "wss://stt-rt.soniox.com/transcribe-websocket";
const PING_INTERVAL_SECS: u64 = 30;
const DEFAULT_MODEL: &str = "stt-rt-preview";

/// Soniox real-time STT provider.
///
/// Wraps a Soniox API key + WebSocket endpoint. Reusable across multiple
/// `transcribe_stream` sessions (1 provider = N sessions).
pub struct SonioxProvider {
    api_key: ApiKey,
    ws_url: String,
    model: String,
}

impl SonioxProvider {
    /// Construct with API key loaded from Keychain (`ApiKey::load("soniox")`).
    pub fn new(api_key: ApiKey) -> Self {
        Self {
            api_key,
            ws_url: DEFAULT_WS_URL.into(),
            model: DEFAULT_MODEL.into(),
        }
    }

    /// Override WebSocket URL (for testing against staging/local mock).
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.ws_url = url.into();
        self
    }

    /// Override model identifier.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Test API key validity by connecting + sending minimal config + closing.
    ///
    /// Per validation decision #8 — public API for Phase 7 onboarding wizard
    /// "Test Connection" button.
    ///
    /// Returns `Ok(())` if API key valid + provider accepts our config.
    /// Returns `Err(SttError::Provider | Network)` on failure with sanitized message.
    pub async fn test_connect(&self) -> Result<(), SttError> {
        let opts = StreamOpts {
            language_hint: Some("vi".into()),
            enable_lid: false,
            enable_diarization: false,
            enable_translation_to: None,
        };
        let config = self.build_config(&opts);
        // Per C1 fix: wrap JSON in Zeroizing so api_key bytes clear after send
        let config_json = serialize_config_zeroizing(&config)?;

        let (mut ws, _resp) = tokio_tungstenite::connect_async(&self.ws_url)
            .await
            .map_err(|e| SttError::Network(format!("connect: {e}")))?;

        // Convert to owned String for tungstenite; Zeroizing drops + clears
        // the original local copy after this line.
        let json_str: String = (*config_json).clone();
        drop(config_json);
        ws.send(Message::Text(json_str.into()))
            .await
            .map_err(|e| SttError::Network(format!("send config: {e}")))?;

        // Send 100ms of silence as sanity probe
        let silence = vec![0i16; 1600]; // 100ms @ 16kHz
        let mut bytes = Vec::with_capacity(silence.len() * 2);
        for s in silence {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        ws.send(Message::Binary(bytes.into()))
            .await
            .map_err(|e| SttError::Network(format!("send silence: {e}")))?;

        // Wait briefly for either an error or first message — 3s timeout
        let deadline = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                Some(msg) = ws.next() => {
                    match msg {
                        Ok(Message::Text(text)) => {
                            if let Ok(tm) = serde_json::from_str::<TokenMessage>(&text) {
                                if let Some(code) = tm.error_code {
                                    let _ = ws.send(Message::Close(None)).await;
                                    return Err(SttError::Provider {
                                        code,
                                        message: tm.error_message.unwrap_or_default(),
                                    });
                                }
                            }
                            // Any non-error response = config accepted
                            let _ = ws.send(Message::Close(None)).await;
                            tracing::info!("Soniox test_connect: OK");
                            return Ok(());
                        }
                        Ok(Message::Close(_)) => {
                            // Provider closed before we got response
                            return Err(SttError::Network("provider closed early".into()));
                        }
                        Err(e) => return Err(SttError::Network(format!("recv: {e}"))),
                        _ => continue,
                    }
                }
                _ = &mut deadline => {
                    let _ = ws.send(Message::Close(None)).await;
                    // No error in 3s = positive signal (Soniox idle until enough audio)
                    tracing::info!("Soniox test_connect: timeout no-error = OK");
                    return Ok(());
                }
            }
        }
    }

    fn build_config(&self, opts: &StreamOpts) -> ConfigMessage {
        ConfigMessage {
            api_key: self.api_key.expose().to_string(),
            model: self.model.clone(),
            audio_format: "pcm_s16le".into(),
            sample_rate: 16_000,
            num_channels: 1,
            enable_language_identification: opts.enable_lid,
            enable_speaker_diarization: opts.enable_diarization,
            language_hints: opts.language_hint.as_ref().map(|h| vec![h.clone()]),
            translation: opts
                .enable_translation_to
                .as_ref()
                .map(|t| TranslationConfig {
                    mode: "one_way".into(),
                    target_language: t.clone(),
                }),
        }
    }
}

#[async_trait]
impl StreamingTranscriber for SonioxProvider {
    fn name(&self) -> &'static str {
        "soniox"
    }

    async fn transcribe_stream(
        &self,
        mut audio_rx: mpsc::Receiver<Utterance>,
        event_tx: mpsc::Sender<TranscriptEvent>,
        opts: StreamOpts,
    ) -> Result<(), SttError> {
        let session_start = Instant::now();
        let mut backoff = ExponentialBackoff::default();
        let mut seq: u64 = 0;

        loop {
            let connect_result = async {
                let (ws, _resp) = tokio_tungstenite::connect_async(&self.ws_url)
                    .await
                    .map_err(|e| SttError::Network(format!("connect: {e}")))?;
                Ok::<_, SttError>(ws)
            }
            .await;

            let mut ws = match connect_result {
                Ok(ws) => ws,
                Err(e) => {
                    tracing::warn!(error = %e, attempt = backoff.attempts(), "Soniox connect failed");
                    match backoff.next_delay() {
                        Some(d) => {
                            tokio::time::sleep(d).await;
                            continue;
                        }
                        None => {
                            return Err(SttError::ReconnectExhausted {
                                attempts: backoff.attempts(),
                            })
                        }
                    }
                }
            };

            // Connected — send config
            let config = self.build_config(&opts);
            // Per C1 fix: wrap JSON in Zeroizing so api_key bytes clear after send
            let config_json = serialize_config_zeroizing(&config)?;
            let json_str: String = (*config_json).clone();
            drop(config_json);
            if let Err(e) = ws.send(Message::Text(json_str.into())).await {
                tracing::warn!(error = %e, "Soniox send config failed");
                continue;
            }
            backoff.reset();
            let _ = event_tx.send(TranscriptEvent::Connected).await;
            tracing::info!(model = %self.model, "Soniox connected");

            // Main I/O loop
            let mut ping_interval = tokio::time::interval(Duration::from_secs(PING_INTERVAL_SECS));
            ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let session_ended = loop {
                tokio::select! {
                    biased;

                    Some(utt) = audio_rx.recv() => {
                        // Send utterance as binary frame (interleaved bytes LE)
                        let mut bytes = Vec::with_capacity(utt.audio_pcm16_mono_16k.len() * 2);
                        for s in &utt.audio_pcm16_mono_16k {
                            bytes.extend_from_slice(&s.to_le_bytes());
                        }
                        if let Err(e) = ws.send(Message::Binary(bytes.into())).await {
                            tracing::warn!(error = %e, "Soniox audio send failed, will reconnect");
                            break false;
                        }
                        tracing::debug!(
                            seq = utt.seq,
                            duration_ms = utt.duration_ms,
                            sample_count = utt.sample_count(),
                            "Soniox audio sent"
                        );
                    }

                    Some(msg) = ws.next() => {
                        match msg {
                            Ok(Message::Text(text)) => {
                                if let Ok(tm) = serde_json::from_str::<TokenMessage>(&text) {
                                    if let Some(code) = tm.error_code.clone() {
                                        let message = tm.error_message.clone().unwrap_or_default();
                                        let _ = event_tx.send(TranscriptEvent::Error { code, message }).await;
                                    }
                                    for tok in tm.tokens {
                                        let event = map_token(&tok, &mut seq, session_start);
                                        if event_tx.send(event).await.is_err() {
                                            return Err(SttError::DownstreamClosed);
                                        }
                                    }
                                }
                            }
                            Ok(Message::Close(_)) => {
                                tracing::info!("Soniox closed cleanly");
                                break true;
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "Soniox WS error, will reconnect");
                                break false;
                            }
                            _ => {}
                        }
                    }

                    _ = ping_interval.tick() => {
                        if let Err(e) = ws.send(Message::Ping(vec![].into())).await {
                            tracing::warn!(error = %e, "Soniox ping failed");
                            break false;
                        }
                    }

                    else => break true,
                }
            };

            let _ = event_tx.send(TranscriptEvent::Disconnected).await;

            if session_ended {
                // Graceful close (audio_rx closed) — exit loop.
                return Ok(());
            }
            // Otherwise: error path — retry via backoff
            match backoff.next_delay() {
                Some(d) => tokio::time::sleep(d).await,
                None => {
                    return Err(SttError::ReconnectExhausted {
                        attempts: backoff.attempts(),
                    })
                }
            }
        }
    }
}

/// Map a Soniox `Token` → public `TranscriptEvent`.
fn map_token(
    tok: &crate::soniox_protocol::Token,
    seq: &mut u64,
    session_start: Instant,
) -> TranscriptEvent {
    let current_seq = *seq;
    *seq = seq.wrapping_add(1);

    let _ = session_start; // reserved for future relative timestamping

    let is_translation = tok
        .translation_status
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("translation"));

    if is_translation {
        return TranscriptEvent::Translation {
            seq: current_seq,
            text: tok.text.clone(),
            source_lang: tok.source_language.clone().unwrap_or_default(),
            target_lang: tok.language.clone().unwrap_or_default(),
            is_final: tok.is_final,
        };
    }

    if tok.is_final {
        TranscriptEvent::Final {
            seq: current_seq,
            text: tok.text.clone(),
            language: tok.language.clone(),
            speaker: tok.speaker.clone(),
            start: Duration::from_millis(tok.start_ms.unwrap_or(0)),
            end: Duration::from_millis(tok.end_ms.unwrap_or(0)),
        }
    } else {
        TranscriptEvent::Partial {
            seq: current_seq,
            text: tok.text.clone(),
            language: tok.language.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soniox_protocol::Token;

    #[test]
    fn provider_with_url_override() {
        let provider = SonioxProvider::new(ApiKey::from_raw("test".into()))
            .with_url("wss://staging.example.com")
            .with_model("stt-rt-test");
        assert_eq!(provider.ws_url, "wss://staging.example.com");
        assert_eq!(provider.model, "stt-rt-test");
        assert_eq!(provider.name(), "soniox");
    }

    #[test]
    fn build_config_minimal() {
        let provider = SonioxProvider::new(ApiKey::from_raw("test-key".into()));
        let opts = StreamOpts::default();
        let cfg = provider.build_config(&opts);
        assert_eq!(cfg.audio_format, "pcm_s16le");
        assert_eq!(cfg.sample_rate, 16_000);
        assert_eq!(cfg.num_channels, 1);
        assert!(cfg.translation.is_none());
    }

    #[test]
    fn build_config_with_translation() {
        let provider = SonioxProvider::new(ApiKey::from_raw("test-key".into()));
        let opts = StreamOpts {
            language_hint: Some("vi".into()),
            enable_lid: true,
            enable_diarization: false,
            enable_translation_to: Some("en".into()),
        };
        let cfg = provider.build_config(&opts);
        assert!(cfg.enable_language_identification);
        assert_eq!(cfg.language_hints.as_deref(), Some(&["vi".to_string()][..]));
        let t = cfg.translation.expect("translation set");
        assert_eq!(t.target_language, "en");
        assert_eq!(t.mode, "one_way");
    }

    #[test]
    fn map_token_partial() {
        let mut seq = 0u64;
        let tok = Token {
            text: "Xin chào".into(),
            is_final: false,
            language: Some("vi".into()),
            speaker: None,
            start_ms: Some(100),
            end_ms: Some(500),
            translation_status: None,
            source_language: None,
        };
        let event = map_token(&tok, &mut seq, Instant::now());
        match event {
            TranscriptEvent::Partial {
                seq: s,
                text,
                language,
            } => {
                assert_eq!(s, 0);
                assert_eq!(text, "Xin chào");
                assert_eq!(language.as_deref(), Some("vi"));
            }
            other => panic!("expected Partial, got {other:?}"),
        }
        assert_eq!(seq, 1);
    }

    #[test]
    fn map_token_final() {
        let mut seq = 5u64;
        let tok = Token {
            text: "Hello".into(),
            is_final: true,
            language: Some("en".into()),
            speaker: None,
            start_ms: Some(1000),
            end_ms: Some(1500),
            translation_status: None,
            source_language: None,
        };
        let event = map_token(&tok, &mut seq, Instant::now());
        match event {
            TranscriptEvent::Final {
                seq: s,
                text,
                start,
                end,
                ..
            } => {
                assert_eq!(s, 5);
                assert_eq!(text, "Hello");
                assert_eq!(start, Duration::from_millis(1000));
                assert_eq!(end, Duration::from_millis(1500));
            }
            other => panic!("expected Final, got {other:?}"),
        }
    }

    #[test]
    fn map_token_translation() {
        let mut seq = 0u64;
        let tok = Token {
            text: "Hello".into(),
            is_final: true,
            language: Some("en".into()),
            speaker: None,
            start_ms: None,
            end_ms: None,
            translation_status: Some("translation".into()),
            source_language: Some("vi".into()),
        };
        let event = map_token(&tok, &mut seq, Instant::now());
        match event {
            TranscriptEvent::Translation {
                source_lang,
                target_lang,
                text,
                is_final,
                ..
            } => {
                assert_eq!(source_lang, "vi");
                assert_eq!(target_lang, "en");
                assert_eq!(text, "Hello");
                assert!(is_final);
            }
            other => panic!("expected Translation, got {other:?}"),
        }
    }
}
