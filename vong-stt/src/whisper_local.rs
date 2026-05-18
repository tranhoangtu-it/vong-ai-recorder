//! Local Whisper transcription provider.
//!
//! Wraps `whisper-rs` (Rust bindings to whisper.cpp). Each utterance from the
//! VAD becomes one Whisper inference run on the tokio blocking pool (whisper.cpp
//! is sync + CPU-heavy). Output is a single `TranscriptEvent::Final` per utterance.
//!
//! ## Model file discovery
//!
//! 1. Env var `VONG_WHISPER_MODEL` if set — explicit path.
//! 2. `<exe_dir>/models/ggml-base.bin` next to the binary.
//! 3. `<data_dir>/Vong/models/ggml-base.bin` via `directories::ProjectDirs`
//!    (Windows: `%LOCALAPPDATA%\Vong\Vong\data\models\ggml-base.bin`).
//!
//! Use `WhisperLocalProvider::resolve_default_model_path()` to apply the search.

use crate::error::SttError;
use crate::events::TranscriptEvent;
use crate::traits::{LiveConfigHandle, LiveSttConfig, StreamOpts, StreamingTranscriber, TargetMode};
use async_trait::async_trait;
use directories::ProjectDirs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use vong_audio::Utterance;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Default model filename — Whisper Base multilingual GGML.
pub const DEFAULT_MODEL_FILENAME: &str = "ggml-base.bin";

/// Local Whisper provider. CPU-only inference, runs on tokio blocking pool.
///
/// Quality vs cost (CPU on a modern laptop, single channel mono 16 kHz):
/// - `ggml-tiny.bin`  ( 75 MB) — fastest, weak VN
/// - `ggml-base.bin`  (142 MB) — recommended default
/// - `ggml-small.bin` (466 MB) — markedly better VN, slower
///
/// Model loading is done once on construction; inference state is per-utterance.
pub struct WhisperLocalProvider {
    ctx: Arc<WhisperContext>,
    model_label: String,
    /// Live, runtime-mutable STT config (source language + target mode).
    /// The streaming loop re-reads this on every utterance, so the UI can
    /// change language pickers and the very next utterance picks them up
    /// without restarting the stream.
    live_config: LiveConfigHandle,
}

impl WhisperLocalProvider {
    /// Load a GGML Whisper model from disk.
    ///
    /// Returns `SttError::Config` if the model file is missing or invalid.
    /// First-call latency dominated by mmap + warmup (~100-300 ms for base).
    pub fn new(model_path: impl AsRef<Path>) -> Result<Self, SttError> {
        let path = model_path.as_ref();
        if !path.exists() {
            return Err(SttError::Config(format!(
                "Whisper model file not found at {}. Download ggml-base.bin from \
                 https://huggingface.co/ggerganov/whisper.cpp and place it there, \
                 or set VONG_WHISPER_MODEL to override.",
                path.display()
            )));
        }

        let params = WhisperContextParameters::default();
        // whisper-rs 0.16: new_with_params takes `&str` model path.
        let path_str = path.to_string_lossy();
        let ctx = WhisperContext::new_with_params(path_str.as_ref(), params)
            .map_err(|e| SttError::Config(format!("whisper model load failed: {e}")))?;

        let model_label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".into());

        tracing::info!(
            model = %model_label,
            path = %path.display(),
            "WhisperLocalProvider: model loaded"
        );

        Ok(Self {
            ctx: Arc::new(ctx),
            model_label,
            live_config: Arc::new(Mutex::new(LiveSttConfig::default())),
        })
    }

    /// Return a handle to the live STT config. Caller mutates this from UI
    /// callbacks; the streaming loop snapshots it per utterance.
    pub fn live_config(&self) -> LiveConfigHandle {
        self.live_config.clone()
    }

    /// Replace the live config handle (useful when caller wants to share one
    /// handle across multiple components, e.g. provider + UI binding).
    pub fn set_live_config(&mut self, cfg: LiveConfigHandle) {
        self.live_config = cfg;
    }

    /// Force the inference backend through a single warmup pass so the
    /// first real utterance doesn't pay the cold-start cost. On Vulkan this
    /// hides the ~6 s shader-pipeline init; on CPU it's a no-op latency-wise
    /// (~30 ms) but still pre-allocates state buffers.
    ///
    /// Blocking. Call from `tokio::task::spawn_blocking` to avoid stalling the
    /// async runtime.
    pub fn warmup(&self) -> Result<(), SttError> {
        // 1 second of silence at 16 kHz — Whisper pads to 30 s internally,
        // so this is the smallest payload that exercises the full graph.
        let silence = vec![0i16; 16_000];
        let n_threads = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4);
        let start = std::time::Instant::now();
        let _ = run_inference(&self.ctx, &silence, Some("vi"), false, n_threads)?;
        tracing::info!(
            elapsed_ms = start.elapsed().as_millis() as u64,
            model = %self.model_label,
            "WhisperLocalProvider: warmup complete — first real utterance will use the warm pipeline"
        );
        Ok(())
    }

    /// Walk the default search ladder and return the first existing path,
    /// or the `data_dir` fallback (which may not exist yet — caller can
    /// download into it).
    pub fn resolve_default_model_path() -> PathBuf {
        // 1. Env override.
        if let Ok(p) = std::env::var("VONG_WHISPER_MODEL") {
            return PathBuf::from(p);
        }

        // 2. Next to the executable: <exe_dir>/models/ggml-base.bin
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let candidate = dir.join("models").join(DEFAULT_MODEL_FILENAME);
                if candidate.exists() {
                    return candidate;
                }
            }
        }

        // 3. AppData fallback. Always returns a path even if the file is missing.
        if let Some(dirs) = ProjectDirs::from("app", "Vong", "Vong") {
            return dirs
                .data_local_dir()
                .join("models")
                .join(DEFAULT_MODEL_FILENAME);
        }

        // Last resort: relative path.
        PathBuf::from("models").join(DEFAULT_MODEL_FILENAME)
    }
}

#[async_trait]
impl StreamingTranscriber for WhisperLocalProvider {
    async fn transcribe_stream(
        &self,
        mut audio_rx: mpsc::Receiver<Utterance>,
        event_tx: mpsc::Sender<TranscriptEvent>,
        opts: StreamOpts,
    ) -> Result<(), SttError> {
        if event_tx.send(TranscriptEvent::Connected).await.is_err() {
            return Err(SttError::DownstreamClosed);
        }

        // Seed live_config from `opts` so callers that don't go through the
        // live-config UI still get the language they specified at startup.
        if let Ok(mut g) = self.live_config.lock() {
            if g.source_language.is_none() {
                g.source_language = opts.language_hint.clone();
            }
        }

        tracing::info!(
            model = %self.model_label,
            language_hint = ?opts.language_hint,
            "WhisperLocalProvider: stream started"
        );

        let n_threads = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4);

        while let Some(utt) = audio_rx.recv().await {
            let seq = utt.seq;
            let duration = Duration::from_millis(utt.duration_ms as u64);
            let is_partial = utt.is_partial;
            // Share audio between blocking tasks via Arc to avoid per-pass clones.
            let audio = Arc::new(utt.audio_pcm16_mono_16k);

            // Snapshot the live config — source language + target mode. We do
            // this PER UTTERANCE so the UI's language pickers take effect
            // immediately on the next sound the user makes.
            let cfg = {
                let guard = self.live_config.lock().expect("live_config poisoned");
                guard.clone()
            };

            if is_partial {
                // ── Partial: only the fast Pass A, no Pass B, no persist. ──
                // Drives the real-time "Bản gốc" column. We don't queue many
                // of these — the VAD only emits one per ~1.5 s of speech.
                let ctx_a = self.ctx.clone();
                let audio_a = audio;
                let event = event_tx.clone();
                let source_lang = cfg.source_language.clone();
                tokio::spawn(async move {
                    let res = tokio::task::spawn_blocking(move || {
                        run_inference(&ctx_a, &audio_a, source_lang.as_deref(), false, n_threads)
                    })
                    .await;
                    match res {
                        Ok(Ok((text, lang))) => {
                            tracing::debug!(
                                seq,
                                text_len = text.len(),
                                lang = lang.as_deref().unwrap_or("?"),
                                "WhisperLocalProvider: pass A (partial) done"
                            );
                            let _ = event
                                .send(TranscriptEvent::Partial { seq, text, language: lang })
                                .await;
                        }
                        Ok(Err(e)) => {
                            tracing::debug!(seq, error = %e, "pass A partial inference failed (best-effort)");
                        }
                        Err(e) => {
                            tracing::warn!(seq, error = %e, "partial spawn_blocking failed");
                        }
                    }
                });
                continue;
            }

            // ── Final: pipelined 2-pass — Pass A then Pass B per `target_mode`. ──
            let ctx_a = self.ctx.clone();
            let ctx_b = self.ctx.clone();
            let audio_a = audio.clone();
            let audio_b = audio;
            let source_lang = cfg.source_language.clone();
            let target_mode = cfg.target_mode.clone();
            let event_a = event_tx.clone();
            let event_b = event_tx.clone();

            tokio::spawn(async move {
                // ── Pass A — source-language transcribe, prioritized for real-time UI feedback ──
                let sl_for_pass_a = source_lang.clone();
                let pass_a = tokio::task::spawn_blocking(move || {
                    run_inference(&ctx_a, &audio_a, sl_for_pass_a.as_deref(), false, n_threads)
                })
                .await;

                match pass_a {
                    Ok(Ok((orig_text, orig_lang))) => {
                        tracing::info!(
                            seq,
                            text_len = orig_text.len(),
                            lang = orig_lang.as_deref().unwrap_or("?"),
                            "WhisperLocalProvider: pass A (original) done"
                        );
                        let _ = event_a
                            .send(TranscriptEvent::Final {
                                seq,
                                text: String::new(),
                                language: None,
                                original_text: orig_text,
                                original_language: orig_lang,
                                speaker: None,
                                start: Duration::ZERO,
                                end: duration,
                            })
                            .await;
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(seq, error = %e, "pass A inference failed");
                        let _ = event_a
                            .send(TranscriptEvent::Error {
                                code: "whisper_inference_pass_a".into(),
                                message: e.to_string(),
                            })
                            .await;
                    }
                    Err(e) => {
                        tracing::warn!(seq, error = %e, "pass A spawn_blocking failed");
                    }
                }

                // ── Pass B — target mode dispatch. ──
                let (target_text, target_lang_code) = match target_mode {
                    TargetMode::Off => {
                        // No second pass — emit empty Translation so UI marks the
                        // row as "done; no translation" instead of "đang dịch…".
                        (String::new(), String::new())
                    }
                    TargetMode::TranslateToEnglish => {
                        let res = tokio::task::spawn_blocking(move || {
                            // language=None lets Whisper auto-detect source;
                            // translate=true forces output to English regardless.
                            run_inference(&ctx_b, &audio_b, None, true, n_threads)
                        })
                        .await;
                        let text = match res {
                            Ok(Ok((t, _))) => t,
                            Ok(Err(e)) => {
                                tracing::warn!(seq, error = %e, "pass B (translate→en) failed");
                                String::new()
                            }
                            Err(e) => {
                                tracing::warn!(seq, error = %e, "pass B spawn_blocking failed");
                                String::new()
                            }
                        };
                        (text, "en".to_string())
                    }
                    TargetMode::Hint(code) => {
                        let code_clone = code.clone();
                        let res = tokio::task::spawn_blocking(move || {
                            run_inference(
                                &ctx_b,
                                &audio_b,
                                Some(&code_clone),
                                false,
                                n_threads,
                            )
                        })
                        .await;
                        let text = match res {
                            Ok(Ok((t, _))) => t,
                            Ok(Err(e)) => {
                                tracing::warn!(seq, code = %code, error = %e, "pass B (hint) failed");
                                String::new()
                            }
                            Err(e) => {
                                tracing::warn!(seq, code = %code, error = %e, "pass B spawn_blocking failed");
                                String::new()
                            }
                        };
                        (text, code)
                    }
                };

                tracing::info!(
                    seq,
                    text_len = target_text.len(),
                    target_lang = target_lang_code,
                    "WhisperLocalProvider: pass B (translation) done"
                );
                let _ = event_b
                    .send(TranscriptEvent::Translation {
                        seq,
                        text: target_text,
                        source_lang: String::new(),
                        target_lang: target_lang_code,
                        is_final: true,
                    })
                    .await;
            });
        }

        let _ = event_tx.send(TranscriptEvent::Disconnected).await;
        tracing::info!("WhisperLocalProvider: stream ended (audio source closed)");
        Ok(())
    }

    fn name(&self) -> &'static str {
        "whisper-local"
    }
}

/// Single utterance → (text, detected_language). Pure blocking; caller wraps in
/// `spawn_blocking`.
///
/// When `language` is `None`, Whisper auto-detects and the second tuple element
/// holds the detected ISO 639-1 code (e.g. `"en"`, `"vi"`). When `language` is
/// `Some(hint)`, the second element echoes that hint.
///
/// `translate = true` switches Whisper into its built-in translation task
/// (output is **always English**, regardless of input language). Use it for the
/// "Bản dịch" column when the user wants English subtitles; combine with
/// `language = None` so the source-language identification happens inside
/// Whisper's translate path. `translate = false` is straight transcription.
fn run_inference(
    ctx: &WhisperContext,
    audio_i16: &[i16],
    language: Option<&str>,
    translate: bool,
    n_threads: i32,
) -> Result<(String, Option<String>), SttError> {
    let pcm_f32: Vec<f32> = audio_i16.iter().map(|&s| s as f32 / 32768.0).collect();

    let mut state = ctx.create_state().map_err(|e| SttError::Provider {
        code: "whisper_state".into(),
        message: e.to_string(),
    })?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(n_threads);
    params.set_translate(translate);
    params.set_language(language);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state
        .full(params, &pcm_f32)
        .map_err(|e| SttError::Provider {
            code: "whisper_inference".into(),
            message: e.to_string(),
        })?;

    // Resolve the language code used for this inference. When the caller passed
    // a hint we already know it; when they passed None, query the state for the
    // language Whisper auto-detected during decode.
    let detected_lang = match language {
        Some(hint) => Some(hint.to_string()),
        None => {
            let lang_id = state.full_lang_id_from_state();
            whisper_rs::get_lang_str(lang_id).map(|s| s.to_string())
        }
    };

    // whisper-rs 0.16: full_n_segments returns i32 directly (infallible).
    let n_segments = state.full_n_segments();

    let mut text = String::new();
    for i in 0..n_segments {
        // whisper-rs 0.16: get_segment(i) → Option<WhisperStateSegment>.
        let Some(seg) = state.get_segment(i) else {
            tracing::warn!(seg_idx = i, "whisper segment index missing — skipping");
            continue;
        };

        // `to_str()` performs strict UTF-8 validation. When the model emits a
        // forced-language hint on cross-lingual audio, individual decoded tokens
        // can land mid-multibyte and fail validation. Skipping just the offending
        // segment is better than aborting the whole utterance — pass B (the
        // translation column) MUST still emit a Translation event so the UI
        // doesn't get stuck on "⏳ đang dịch…".
        match seg.to_str() {
            Ok(chunk) => text.push_str(chunk),
            Err(e) => {
                tracing::warn!(
                    seg_idx = i,
                    error = %e,
                    "whisper segment has invalid UTF-8 — skipping segment (best-effort decode)"
                );
            }
        }
    }

    Ok((text.trim().to_string(), detected_lang))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_default_model_path_returns_a_path() {
        // Whatever the search ladder yields, it must not panic and must be non-empty.
        let p = WhisperLocalProvider::resolve_default_model_path();
        assert!(!p.as_os_str().is_empty());
    }

    #[test]
    fn missing_model_returns_config_error() {
        // Pointing at a definitely-non-existent file should be a config error
        // before whisper-rs is even called.
        //
        // We can't use `.unwrap_err()` because `WhisperLocalProvider` holds an
        // `Arc<WhisperContext>` that doesn't implement `Debug`.
        match WhisperLocalProvider::new("E:/nonexistent/no-such-model.bin") {
            Ok(_) => panic!("expected SttError::Config, got Ok"),
            Err(SttError::Config(msg)) => assert!(msg.contains("not found")),
            Err(other) => panic!("expected Config error, got {other:?}"),
        }
    }
}
