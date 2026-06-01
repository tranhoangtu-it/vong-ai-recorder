//! OpenAI gpt-realtime transcription provider.
//!
//! WebSocket protocol: `wss://api.openai.com/v1/realtime?intent=transcription`
//! Auth: `Authorization: Bearer <key>` + `OpenAI-Beta: realtime=v1`
//! Audio: PCM16 mono 16 kHz, base64-encoded chunks in `input_audio_buffer.append`.
//! Events we care about (subset of the full Realtime event set):
//!   - `input_audio_buffer.committed` — server assigns `item_id` to our commit
//!   - `conversation.item.input_audio_transcription.delta` — word-by-word partial
//!   - `conversation.item.input_audio_transcription.completed` — final transcript
//!   - `error` — recoverable provider error
//!
//! We map server-side `item_id` to our `Utterance::seq` via a FIFO queue:
//! every commit we send pushes a seq, every `input_audio_buffer.committed`
//! event drains the oldest pending seq and pins it to the `item_id` the
//! server just minted. Subsequent delta/completed events for that item_id
//! re-use the pinned seq, so UI-side upsert-by-seq stays consistent across
//! providers.

use crate::api_key::ApiKey;
use crate::error::SttError;
use crate::events::TranscriptEvent;
use crate::traits::{StreamOpts, StreamingTranscriber};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use vong_audio::Utterance;

/// Default WebSocket endpoint. `intent=transcription` selects the cheaper,
/// transcription-only session type (no GPT response generation).
const DEFAULT_WS_URL: &str = "wss://api.openai.com/v1/realtime?intent=transcription";

/// Default transcription model. `gpt-4o-mini-transcribe` is the cheap tier;
/// callers can switch to `gpt-4o-transcribe` (better quality) or `whisper-1`
/// (highest accuracy, no streaming partials).
const DEFAULT_MODEL: &str = "gpt-4o-mini-transcribe";

/// OpenAI Realtime transcription provider. Reusable across multiple
/// `transcribe_stream` sessions.
pub struct OpenAIRealtimeProvider {
    api_key: ApiKey,
    ws_url: String,
    model: String,
    /// Pre-built `session.update.instructions` string for vocabulary injection.
    ///
    /// Built via `build_openai_instructions()` and injected into the
    /// `transcription_session.update` message at session open. Empty string = no-op.
    ///
    /// Privacy: this string contains user vocabulary — never log it.
    dictionary_instructions: String,
}

impl OpenAIRealtimeProvider {
    /// Construct with API key (load via `ApiKey::load("openai-realtime")`).
    pub fn new(api_key: ApiKey) -> Self {
        Self {
            api_key,
            ws_url: DEFAULT_WS_URL.into(),
            model: DEFAULT_MODEL.into(),
            dictionary_instructions: String::new(),
        }
    }

    /// Override the WebSocket URL (for testing against a mock).
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.ws_url = url.into();
        self
    }

    /// Override the transcription model id.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Inject a pre-built vocabulary instructions string into the session config.
    ///
    /// Use `build_openai_instructions(&entries)` to construct the string from a
    /// `Vec<DictEntry>`. Applied once at WebSocket session open — changing the
    /// dictionary after session start requires a restart.
    ///
    /// Privacy: the instructions string contains user vocabulary — never log it.
    pub fn with_dictionary_instructions(mut self, instructions: String) -> Self {
        self.dictionary_instructions = instructions;
        self
    }
}

#[async_trait]
impl StreamingTranscriber for OpenAIRealtimeProvider {
    async fn transcribe_stream(
        &self,
        mut audio_rx: mpsc::Receiver<Utterance>,
        event_tx: mpsc::Sender<TranscriptEvent>,
        opts: StreamOpts,
    ) -> Result<(), SttError> {
        // ── Build WebSocket upgrade request with auth + beta headers ──
        let mut request = self
            .ws_url
            .clone()
            .into_client_request()
            .map_err(|e| SttError::Provider {
                code: "ws_request_build".into(),
                message: e.to_string(),
            })?;
        {
            let headers = request.headers_mut();
            headers.insert(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose())).map_err(
                    |e| SttError::Credential(format!("invalid api key encoding: {e}")),
                )?,
            );
            headers.insert("OpenAI-Beta", HeaderValue::from_static("realtime=v1"));
        }

        let (ws, _resp) = connect_async(request)
            .await
            .map_err(|e| SttError::Network(format!("OpenAI WS connect: {e}")))?;

        if event_tx.send(TranscriptEvent::Connected).await.is_err() {
            return Err(SttError::DownstreamClosed);
        }

        tracing::info!(
            model = %self.model,
            language_hint = ?opts.language_hint,
            "OpenAIRealtimeProvider: connected"
        );

        let (mut write, mut read) = ws.split();

        // ── Configure the transcription session ──
        // turn_detection=null because our own VAD already packages utterances;
        // server-side VAD would double-segment and confuse the seq mapping.
        //
        // `instructions` is injected from the user's dictionary vocabulary.
        // An empty string is a safe no-op for the OpenAI Realtime API.
        // Privacy: log only whether instructions are non-empty, never the content.
        let instructions = self.dictionary_instructions.clone();
        if !instructions.is_empty() {
            tracing::debug!(
                instructions_len = instructions.len(),
                "OpenAIRealtimeProvider: dictionary instructions injected"
            );
        }
        let session_config = json!({
            "type": "transcription_session.update",
            "session": {
                "input_audio_format": "pcm16",
                "input_audio_transcription": {
                    "model": self.model.clone(),
                    "language": opts.language_hint.clone(),
                },
                "turn_detection": serde_json::Value::Null,
                "instructions": instructions,
            }
        });
        write
            .send(Message::Text(session_config.to_string().into()))
            .await
            .map_err(|e| SttError::Network(format!("session.update send: {e}")))?;

        // ── seq mapping shared between sender + receiver tasks ──
        let mapping: Arc<Mutex<Mapper>> = Arc::new(Mutex::new(Mapper::default()));

        // ── Sender task: drains audio_rx → input_audio_buffer.append + commit ──
        let mapping_send = mapping.clone();
        let send_task = tokio::spawn(async move {
            while let Some(utt) = audio_rx.recv().await {
                if utt.is_partial {
                    // OpenAI Realtime streams its own partials from delta
                    // events — sending our 1.5s partial snapshots on top
                    // would just produce duplicate transcription requests.
                    continue;
                }
                let seq = utt.seq;

                // PCM16 little-endian byte stream
                let pcm_bytes: Vec<u8> = utt
                    .audio_pcm16_mono_16k
                    .iter()
                    .flat_map(|s| s.to_le_bytes())
                    .collect();
                let b64_audio = B64.encode(&pcm_bytes);

                let append_msg = json!({
                    "type": "input_audio_buffer.append",
                    "audio": b64_audio,
                });
                if write
                    .send(Message::Text(append_msg.to_string().into()))
                    .await
                    .is_err()
                {
                    tracing::warn!(seq, "OpenAI WS append send failed — sender exiting");
                    return;
                }

                let commit_msg = json!({ "type": "input_audio_buffer.commit" });
                if write
                    .send(Message::Text(commit_msg.to_string().into()))
                    .await
                    .is_err()
                {
                    tracing::warn!(seq, "OpenAI WS commit send failed — sender exiting");
                    return;
                }

                mapping_send.lock().await.enqueue(seq);
            }
        });

        // ── Receiver loop: parses events → emits TranscriptEvent ──
        let lang_hint = opts.language_hint.clone();
        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    let val: serde_json::Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::debug!(error = %e, "OpenAI: skip non-JSON event");
                            continue;
                        }
                    };
                    let event_type = val["type"].as_str().unwrap_or("");
                    match event_type {
                        "input_audio_buffer.committed" => {
                            if let Some(item_id) = val["item_id"].as_str() {
                                let pinned =
                                    mapping.lock().await.link(item_id);
                                if let Some(seq) = pinned {
                                    tracing::debug!(item_id, seq, "OpenAI: buffer committed");
                                }
                            }
                        }
                        "conversation.item.input_audio_transcription.delta" => {
                            let item_id = val["item_id"].as_str().unwrap_or("");
                            let delta = val["delta"].as_str().unwrap_or("");
                            if delta.is_empty() {
                                continue;
                            }
                            let seq = mapping.lock().await.lookup(item_id);
                            if let Some(seq) = seq {
                                let _ = event_tx
                                    .send(TranscriptEvent::Partial {
                                        seq,
                                        text: delta.to_string(),
                                        language: lang_hint.clone(),
                                    })
                                    .await;
                            }
                        }
                        "conversation.item.input_audio_transcription.completed" => {
                            let item_id = val["item_id"].as_str().unwrap_or("");
                            let transcript = val["transcript"].as_str().unwrap_or("");
                            let seq = mapping.lock().await.lookup(item_id);
                            if let Some(seq) = seq {
                                // OpenAI Realtime in transcription-only mode does NOT
                                // do cross-lingual translation. We emit the final
                                // transcript as both Final (Bản gốc) and Translation
                                // (Bản dịch) so the UI's right column doesn't sit on
                                // the "đang dịch…" placeholder. Real translation via
                                // OpenAI would require a separate request.
                                let _ = event_tx
                                    .send(TranscriptEvent::Final {
                                        seq,
                                        text: String::new(),
                                        language: None,
                                        original_text: transcript.to_string(),
                                        original_language: lang_hint.clone(),
                                        speaker: None,
                                        start: Duration::ZERO,
                                        end: Duration::ZERO,
                                    })
                                    .await;
                                let _ = event_tx
                                    .send(TranscriptEvent::Translation {
                                        seq,
                                        text: transcript.to_string(),
                                        source_lang: String::new(),
                                        target_lang: lang_hint
                                            .clone()
                                            .unwrap_or_default(),
                                        is_final: true,
                                    })
                                    .await;
                            }
                        }
                        "error" => {
                            let code = val["error"]["code"]
                                .as_str()
                                .unwrap_or("unknown")
                                .to_string();
                            let message = val["error"]["message"]
                                .as_str()
                                .unwrap_or("")
                                .to_string();
                            tracing::warn!(code, message, "OpenAI Realtime: error event");
                            let _ = event_tx
                                .send(TranscriptEvent::Error { code, message })
                                .await;
                        }
                        _ => {
                            // session.created / conversation.item.created /
                            // input_audio_buffer.speech_started / etc. — ignored.
                        }
                    }
                }
                Ok(Message::Close(_)) => {
                    tracing::info!("OpenAI Realtime: WebSocket closed by server");
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "OpenAI Realtime: WebSocket recv error");
                    break;
                }
            }
        }

        send_task.abort();
        let _ = event_tx.send(TranscriptEvent::Disconnected).await;
        tracing::info!("OpenAIRealtimeProvider: stream ended");
        Ok(())
    }

    fn name(&self) -> &'static str {
        "openai-realtime"
    }
}

/// FIFO mapping between our `Utterance::seq` and OpenAI server-issued
/// `item_id`. See module doc for the protocol it implements.
#[derive(Default)]
struct Mapper {
    /// Seqs already sent + committed by us, waiting for the server's
    /// `input_audio_buffer.committed` event to assign them an `item_id`.
    pending: VecDeque<u64>,
    /// Resolved `item_id` → our `seq` lookup table.
    by_item: HashMap<String, u64>,
}

impl Mapper {
    fn enqueue(&mut self, seq: u64) {
        self.pending.push_back(seq);
    }

    fn link(&mut self, item_id: &str) -> Option<u64> {
        let seq = self.pending.pop_front()?;
        self.by_item.insert(item_id.to_string(), seq);
        Some(seq)
    }

    fn lookup(&self, item_id: &str) -> Option<u64> {
        self.by_item.get(item_id).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapper_links_and_lookups_in_order() {
        let mut m = Mapper::default();
        m.enqueue(10);
        m.enqueue(11);
        m.enqueue(12);
        assert_eq!(m.link("item-a"), Some(10));
        assert_eq!(m.link("item-b"), Some(11));
        assert_eq!(m.lookup("item-a"), Some(10));
        assert_eq!(m.lookup("item-b"), Some(11));
        assert_eq!(m.lookup("item-c"), None);
        assert_eq!(m.link("item-c"), Some(12));
        assert_eq!(m.link("item-d"), None); // queue drained
    }

    #[test]
    fn provider_construction() {
        let key = ApiKey::from_raw("sk-test".into());
        let p = OpenAIRealtimeProvider::new(key)
            .with_url("wss://example.com")
            .with_model("test-model");
        assert_eq!(p.ws_url, "wss://example.com");
        assert_eq!(p.model, "test-model");
        assert_eq!(p.name(), "openai-realtime");
    }
}
