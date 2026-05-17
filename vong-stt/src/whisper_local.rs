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
use crate::traits::{StreamOpts, StreamingTranscriber};
use async_trait::async_trait;
use directories::ProjectDirs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
        })
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
        let _ = run_inference(&self.ctx, &silence, Some("vi"), n_threads)?;
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

        tracing::info!(
            model = %self.model_label,
            language_hint = ?opts.language_hint,
            "WhisperLocalProvider: stream started"
        );

        let n_threads = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4);

        while let Some(utt) = audio_rx.recv().await {
            let ctx = self.ctx.clone();
            let lang = opts.language_hint.clone();
            let seq = utt.seq;
            let duration = Duration::from_millis(utt.duration_ms as u64);
            let audio = utt.audio_pcm16_mono_16k;

            // Inference is sync + CPU-heavy. Run on blocking pool.
            let infer = tokio::task::spawn_blocking(move || {
                run_inference(&ctx, &audio, lang.as_deref(), n_threads)
            })
            .await
            .map_err(|e| SttError::Provider {
                code: "whisper_spawn".into(),
                message: e.to_string(),
            })?;

            let text = match infer {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(seq, error = %e, "Whisper inference failed for utterance");
                    let _ = event_tx
                        .send(TranscriptEvent::Error {
                            code: "whisper_inference".into(),
                            message: e.to_string(),
                        })
                        .await;
                    continue;
                }
            };

            tracing::info!(
                seq,
                duration_ms = duration.as_millis() as u64,
                text_len = text.len(),
                "WhisperLocalProvider: transcribed"
            );

            // start/end relative to session start — Phase 6 storage will anchor properly.
            // For Phase 4 demo, treat each utterance as 0..duration.
            let event = TranscriptEvent::Final {
                seq,
                text,
                language: opts.language_hint.clone(),
                speaker: None,
                start: Duration::ZERO,
                end: duration,
            };

            if event_tx.send(event).await.is_err() {
                return Err(SttError::DownstreamClosed);
            }
        }

        let _ = event_tx.send(TranscriptEvent::Disconnected).await;
        tracing::info!("WhisperLocalProvider: stream ended (audio source closed)");
        Ok(())
    }

    fn name(&self) -> &'static str {
        "whisper-local"
    }
}

/// Single utterance → Whisper text. Pure blocking; caller wraps in `spawn_blocking`.
fn run_inference(
    ctx: &WhisperContext,
    audio_i16: &[i16],
    language: Option<&str>,
    n_threads: i32,
) -> Result<String, SttError> {
    let pcm_f32: Vec<f32> = audio_i16.iter().map(|&s| s as f32 / 32768.0).collect();

    let mut state = ctx.create_state().map_err(|e| SttError::Provider {
        code: "whisper_state".into(),
        message: e.to_string(),
    })?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(n_threads);
    params.set_translate(false);
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

    // whisper-rs 0.16: full_n_segments returns i32 directly (infallible).
    let n_segments = state.full_n_segments();

    let mut text = String::new();
    for i in 0..n_segments {
        // whisper-rs 0.16: get_segment(i) → Option<WhisperStateSegment>.
        // The segment's text accessor name varies; we accept the first that compiles.
        let seg = state.get_segment(i).ok_or_else(|| SttError::Provider {
            code: "whisper_segment".into(),
            message: format!("segment index {i} missing"),
        })?;
        let chunk = seg.to_str().map_err(|e| SttError::Provider {
            code: "whisper_segment_text".into(),
            message: e.to_string(),
        })?;
        text.push_str(chunk);
    }

    Ok(text.trim().to_string())
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
