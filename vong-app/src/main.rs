//! Vọng AI Recorder — Main entry point.
//!
//! Phase 0: Slint hello window with design tokens preview.
//! Phase 1 + 3 + 4 + 6 (Windows-first, local STT, on-disk history):
//!   cpal WASAPI mic capture → rtrb SPSC ring → rubato sinc resampler
//!   → earshot VAD FSM → Utterance pack → Whisper.cpp local inference
//!   → TranscriptEvent::Final → SQLite FTS5 (NFC-normalized for VN tone search).
//! UI shows live peak meter, utterance counter, latest transcript, and session id.

// On Windows release builds, mark the binary as a GUI subsystem app so
// double-clicking vong.exe does NOT spawn a console window alongside the
// Slint UI. Dev builds (`cargo run`) keep the console so stderr logs stay
// visible during development.
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod log_init;
mod tray;
mod wizard;

use cpal::traits::DeviceTrait;
use rtrb::RingBuffer;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vong_audio::{
    default_input_device, find_device, list_all_devices, negotiate_config, run_resampler,
    run_vad_fsm, start_capture, AudioError, CaptureHandle, DeviceInfo, DeviceKind, OverflowCounter,
    PeakMeter, ResampleConfig, Utterance, VadConfig,
};
use vong_storage::{
    default_db_path, export_markdown, finalize_session, insert_segment, insert_session,
    list_recent_sessions, open_default, search_transcripts, Connection, NewSegment, NewSession,
    SearchHit, Session,
};
use vong_transcribe::{
    model_dl, ApiKey, DlError, DownloadProgress as DlProgress, DownloadState, LiveConfigHandle,
    OpenAIRealtimeProvider, ProviderMode, SonioxProvider, StreamOpts, StreamingTranscriber,
    TargetMode, TranscriptEvent, WhisperLocalProvider,
};

slint::include_modules!();

/// Holds every resource the running audio pipeline depends on. Dropping this
/// struct stops the active capture, kills the resampler + VAD + Whisper tasks,
/// and tears down the tokio runtime. `active_source` is hot-swappable when the
/// user picks a different device from the UI — the rest of the pipeline (VAD,
/// Whisper, storage, Slint timer) stays alive across swaps.
struct AudioBundle {
    _active_source: Rc<RefCell<Option<AudioSource>>>,
    _timer: slint::Timer,
    _runtime: tokio::runtime::Runtime,
    session: Option<SessionContext>,
}

/// A live capture chain: the cpal stream (owns its own RT thread) plus the
/// resampler task that drains its ring buffer into the shared `audio_tx`.
/// Dropping the source stops the cpal stream and aborts the resampler task.
struct AudioSource {
    _handle: CaptureHandle,
    _resampler_abort: tokio::task::AbortHandle,
    sample_rate: u32,
    channels: u16,
    device_name: String,
    kind: DeviceKind,
}

/// Per-recording session: SQLite connection + session id from `sessions` table.
/// Shared across whisper event consumer + shutdown finalize via `Arc`.
struct SessionContext {
    conn: Arc<Mutex<Connection>>,
    session_id: i64,
    started_at_ms: i64,
}

impl SessionContext {
    fn clone_for_task(&self) -> Self {
        Self {
            conn: self.conn.clone(),
            session_id: self.session_id,
            started_at_ms: self.started_at_ms,
        }
    }
}

/// Shared state pushed from background tasks into the Slint Timer.
#[derive(Default)]
struct PipelineState {
    utterance_count: AtomicU64,
    last_seq: AtomicU64,
    last_duration_ms: AtomicU32,
    last_transcript: Mutex<String>,
    last_language: Mutex<String>,
    whisper_loaded: std::sync::atomic::AtomicBool,
    /// True while the Whisper warmup pass is running on the blocking pool.
    /// Drops to false once the first dummy inference completes — UI uses this
    /// to show a "🔥 starting GPU" hint instead of the silent "Listening…".
    whisper_warming_up: std::sync::atomic::AtomicBool,
    /// SQLite sessions.id; 0 if storage init failed (segments are not persisted).
    session_id: AtomicU64,
    /// Number of `transcript_segments` rows successfully inserted this run.
    segments_persisted: AtomicU64,
    /// Soniox-style scrolling transcript stream. Each new Final event appends
    /// a line; UI Timer copies into the Slint VecModel for rendering.
    transcript_stream: Mutex<Vec<TranscriptStreamLine>>,
    /// Generation counter for `transcript_stream` — bumped on any mutation
    /// (append, in-place text update). The Slint Timer compares this against
    /// the last value it pushed and only re-renders when it changes.
    /// Without this, the length-only check missed pass-B text updates on
    /// existing rows.
    transcript_stream_gen: AtomicU64,
    /// User-controlled record toggle. When false, the relay task between
    /// VAD and Whisper drops every utterance — Whisper receives no input
    /// and the UI stays idle. Toggled by the mic button between the two
    /// transcript columns. Default `false` (off until explicit click).
    is_recording: std::sync::atomic::AtomicBool,
    /// Provider connection state. For Whisper local, true after the model
    /// is loaded. For Soniox/OpenAI, true after the WebSocket Connected
    /// event lands and false after Disconnected. Drives the bottom-left
    /// status dot (green/gray).
    provider_online: std::sync::atomic::AtomicBool,
}

/// Local mirror of Slint's `TranscriptLine` struct for in-Rust storage.
/// Converted to the Slint-generated struct only when pushing to the model.
///
/// Two parallel renditions per row:
/// - `text` / `lang` — language-hinted Whisper pass ("Bản phiên âm")
/// - `original_text` / `original_lang` — auto-detect Whisper pass ("Bản gốc")
///
/// `translation_done` distinguishes "pass B still running" (UI shows the
/// placeholder) from "pass B finished but produced empty text" (UI shows a
/// "no result" marker). Without this flag, an empty `text` is ambiguous and
/// the placeholder stays forever on edge cases.
#[derive(Debug, Clone)]
struct TranscriptStreamLine {
    seq: i32,
    time: String,
    lang: String,
    text: String,
    original_lang: String,
    original_text: String,
    duration_ms: i32,
    translation_done: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Bind the log-file flush guard for the entire app lifetime — dropping it
    // flushes any pending log lines from the rolling file appender.
    let _log_guard = log_init::init_logging();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Vọng starting");

    // Clean up orphaned .part files from prior crashed/cancelled downloads.
    // Runs synchronously before UI init — takes <1 ms on an empty models dir.
    let models_dir = resolve_models_dir();
    let cleaned = model_dl::cleanup_stale_parts(&models_dir);
    if cleaned > 0 {
        tracing::info!(stale_parts_cleaned = cleaned, "startup: removed stale model part files");
    }

    let ui = AppWindow::new()?;

    // Audio-source picker: enumerate devices + populate dropdown.
    populate_audio_source_picker(&ui);

    // Language pickers: populate dropdown models + set defaults.
    populate_language_pickers(&ui);

    // Provider picker (Whisper local / Soniox / OpenAI Realtime).
    populate_provider_picker(&ui);
    wire_provider_picker_callbacks(&ui);

    // ── First-run wizard startup detection ────────────────────────────────────
    // Read wizard progress from onboarded.txt. If incomplete or absent, show the
    // wizard overlay. If already complete (including legacy alpha marker files),
    // skip straight to the main view.
    {
        let progress = wizard::read_progress();
        let (initial_view, initial_step) = match progress {
            wizard::WizardProgress::Complete => ("main", 0i32),
            wizard::WizardProgress::NotStarted => ("wizard", 0i32),
            wizard::WizardProgress::Resume(idx) => {
                // Resume at last_completed + 1, clamped to step 6
                let next = (idx + 1).min(6);
                tracing::info!(
                    resume_step = next,
                    "wizard: resuming from last completed step"
                );
                ("wizard", next as i32)
            }
        };
        ui.set_current_view(slint::SharedString::from(initial_view));
        ui.set_wizard_step(initial_step);
        tracing::info!(
            view = initial_view,
            step = initial_step,
            "startup: initial view determined from wizard progress"
        );
    }

    // Wire wizard navigation callbacks.
    wire_wizard_callbacks(&ui, &models_dir);

    let app_started_at = std::time::Instant::now();

    let audio = match init_audio(&ui, app_started_at) {
        Ok(bundle) => Some(bundle),
        Err(e) => {
            let msg = format!("Microphone unavailable — {e}");
            tracing::warn!(error = %e, "audio pipeline init failed");
            ui.set_audio_status(msg.into());
            None
        }
    };

    // System tray + global hotkey (Phase 5 partial). Non-fatal if setup fails
    // (some Windows display sessions disable the notification area).
    let _tray = match tray::init(&ui) {
        Ok(b) => Some(b),
        Err(e) => {
            tracing::warn!(error = %e, "tray init failed — running without tray icon");
            None
        }
    };

    // ── Model download shared state (Phase 2) ─────────────────────────────
    // `dl_progress` is read by the Slint 30 Hz timer and written by the
    // download task. `dl_cancel` is set by the cancel button callback.
    // Phase 3 wizard will mount `ModelDownloadCard` and wire these handles.
    let dl_progress: Arc<Mutex<DlProgress>> = Arc::new(Mutex::new(DlProgress::default()));
    let dl_cancel: Arc<std::sync::atomic::AtomicBool> =
        Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Wire start/cancel callbacks that Phase 3 will invoke from the wizard.
    // They are registered here so the Slint property bridge is alive for the
    // full app lifetime; the wizard just calls `ui.on_start_model_download`.
    wire_model_download_callbacks(&ui, dl_progress.clone(), dl_cancel.clone(), &models_dir);

    // ── Download-progress + model-ready mirror timer ──────────────────────────
    // Pushes `dl_progress` into `ui.download-progress` at 30 Hz (same cadence as
    // the audio-pipeline timer in init_audio). Also checks whether a model is
    // present on disk for the wizard step-4 "Next" enable gate.
    let ui_weak_dl = ui.as_weak();
    let dl_progress_timer = dl_progress.clone();
    let models_dir_timer = models_dir.clone();
    let _dl_timer = {
        let t = slint::Timer::default();
        t.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(33),
            move || {
                let Some(ui) = ui_weak_dl.upgrade() else { return };

                // Mirror download progress struct into Slint property.
                if let Ok(p) = dl_progress_timer.lock() {
                    ui.set_download_progress(DownloadProgress {
                        state: slint::SharedString::from(p.state.as_str()),
                        bytes_downloaded: p.bytes.min(i32::MAX as u64) as i32,
                        total_bytes: p.total.unwrap_or(0).min(i32::MAX as u64) as i32,
                        eta_secs: p.eta_secs.unwrap_or(0) as i32,
                        percent: p.percent(),
                        error_message: slint::SharedString::from(&p.error_msg),
                    });
                }

                // model-ready: check whether any ggml-*.bin file exists in models dir.
                let ready = models_dir_timer
                    .read_dir()
                    .ok()
                    .map(|mut entries| {
                        entries.any(|e| {
                            e.ok()
                                .and_then(|e| e.file_name().into_string().ok())
                                .map(|n| n.starts_with("ggml-") && n.ends_with(".bin"))
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false);
                ui.set_model_ready(ready);
            },
        );
        t
    };

    ui.run()?;
    // Keep _dl_timer alive until here (timer drops → stops).
    drop(_dl_timer);

    // Finalize the session row before tearing down the runtime so end_ts/duration
    // get written. This is best-effort — if the DB lock can't be acquired (e.g.
    // another task is mid-insert), we log and move on rather than blocking exit.
    if let Some(ref bundle) = audio {
        if let Some(session) = &bundle.session {
            match session.conn.try_lock() {
                Ok(guard) => match finalize_session(&guard, session.session_id) {
                    Ok(()) => tracing::info!(
                        session_id = session.session_id,
                        "session finalized on shutdown"
                    ),
                    Err(e) => tracing::warn!(error = ?e, "finalize_session failed"),
                },
                Err(_) => tracing::warn!(
                    session_id = session.session_id,
                    "session_id finalize skipped — DB locked at shutdown"
                ),
            }
        }
    }
    drop(audio);

    tracing::info!("Vọng shutting down");
    Ok(())
}

fn init_audio(
    ui: &AppWindow,
    app_started_at: std::time::Instant,
) -> Result<AudioBundle, AudioError> {
    // Shared sinks used by every audio source we ever start.
    let peak = PeakMeter::new();
    let overflow = OverflowCounter::new();

    // Dedicated tokio runtime. Whisper inference needs a blocking pool; 2 worker
    // threads + spawn_blocking is enough for resampler + VAD + STT driver + event
    // consumer + audio-source hot-swap.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("vong-audio-rt")
        .enable_all()
        .build()
        .map_err(|e| AudioError::StreamBuild(format!("tokio runtime init: {e}")))?;

    // resampler → audio chunks @ 16 kHz mono i16. Channel created once and shared
    // across audio sources so VAD/Whisper downstream stay connected through swaps.
    let (audio_tx, audio_rx) = tokio::sync::mpsc::channel::<Vec<i16>>(64);

    // First source — what `resolve_audio_device()` returned from config / default.
    let initial_source =
        build_audio_source_from_config(&runtime, audio_tx.clone(), &peak, &overflow)?;
    let device_name = initial_source.device_name.clone();
    let sample_rate = initial_source.sample_rate;
    let channels = initial_source.channels;
    let initial_kind = initial_source.kind;
    let active_source: Rc<RefCell<Option<AudioSource>>> =
        Rc::new(RefCell::new(Some(initial_source)));

    // VAD FSM emits to an INTERNAL channel. A relay task forwards into the
    // external channel that Whisper actually consumes — gated by the
    // `is_recording` toggle so audio doesn't reach the model when the user
    // hasn't clicked the mic button.
    let (utt_internal_tx, mut utt_internal_rx) = tokio::sync::mpsc::channel::<Utterance>(32);
    let (utt_external_tx, utt_external_rx) = tokio::sync::mpsc::channel::<Utterance>(32);
    let vad_cfg = VadConfig::default();
    runtime.spawn(async move {
        if let Err(e) = run_vad_fsm(audio_rx, utt_internal_tx, vad_cfg).await {
            tracing::error!(error = ?e, "VAD FSM exited with error");
        } else {
            tracing::info!("VAD FSM exited cleanly");
        }
    });

    let state = Arc::new(PipelineState::default());

    // Relay: gate by `is_recording`. Drops partials too — we explicitly
    // DON'T want stale partials from the moment recording flips on; the
    // user expects fresh utterances only.
    let state_for_relay = state.clone();
    runtime.spawn(async move {
        while let Some(utt) = utt_internal_rx.recv().await {
            if state_for_relay.is_recording.load(Ordering::Acquire) {
                if utt_external_tx.send(utt).await.is_err() {
                    tracing::warn!("relay: external utt channel closed");
                    break;
                }
            } else {
                tracing::trace!(
                    seq = utt.seq,
                    is_partial = utt.is_partial,
                    "relay: dropped (recording=off)"
                );
            }
        }
    });

    // Rename for clarity — downstream code consumed `utt_rx`.
    let utt_rx = utt_external_rx;

    // Wire the mic-button toggle. Default OFF — `state.is_recording` starts
    // false (AtomicBool::default), so the relay drops everything until the
    // user explicitly clicks. The button itself owns the UI state via
    // `in-out recording`; this callback just mirrors it into Rust.
    {
        let state_for_record = state.clone();
        ui.on_record_toggled(move |on| {
            state_for_record
                .is_recording
                .store(on, Ordering::Release);
            tracing::info!(recording = on, "record button toggled");
        });
    }

    // Open SQLite + create a session row. Storage failure is non-fatal — the
    // pipeline runs without persistence (segments_persisted stays at 0).
    let session = init_storage(&device_name, state.clone());

    // Decide which STT engine to drive the pipeline. Read at startup; the UI
    // picker writes the config but a restart is required to swap providers.
    let provider_mode = read_provider_mode();
    tracing::info!(provider = %provider_mode.id(), "STT provider mode selected");
    ui.set_current_provider_mode(provider_label(provider_mode).into());
    ui.set_provider_is_cloud(provider_mode != ProviderMode::LocalWhisper);
    // Short label for the footer status indicator — strip the emoji prefix
    // off the picker label for a tighter footer.
    let short = match provider_mode {
        ProviderMode::LocalWhisper => "Whisper local",
        ProviderMode::SonioxCloud => "Soniox",
        ProviderMode::OpenAIRealtime => "OpenAI Realtime",
    };
    ui.set_provider_status_label(short.into());

    match provider_mode {
        ProviderMode::SonioxCloud => match ApiKey::load("soniox") {
            Ok(key) => {
                tracing::info!("Soniox API key loaded from keychain — using SonioxProvider");
                state
                    .whisper_loaded
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                let provider = Arc::new(SonioxProvider::new(key));
                spawn_soniox_pipeline(
                    &runtime,
                    provider,
                    utt_rx,
                    state.clone(),
                    session.as_ref().map(SessionContext::clone_for_task),
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "Soniox selected but no API key in keychain — falling back to utterance counter. Set the key via the UI."
                );
                spawn_utterance_counter(&runtime, utt_rx, state.clone());
            }
        },
        ProviderMode::OpenAIRealtime => match ApiKey::load("openai-realtime") {
            Ok(key) => {
                tracing::info!(
                    "OpenAI API key loaded from keychain — using OpenAIRealtimeProvider"
                );
                state
                    .whisper_loaded
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                let provider = Arc::new(OpenAIRealtimeProvider::new(key));
                spawn_openai_pipeline(
                    &runtime,
                    provider,
                    utt_rx,
                    state.clone(),
                    session.as_ref().map(SessionContext::clone_for_task),
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "OpenAI Realtime selected but no API key in keychain — falling back to utterance counter. Set the key via the UI."
                );
                spawn_utterance_counter(&runtime, utt_rx, state.clone());
            }
        },
        ProviderMode::LocalWhisper => {
            // Try to load Whisper. If model missing, fall back to utterance-only counter.
            let model_path = WhisperLocalProvider::resolve_default_model_path();
            let whisper_result = WhisperLocalProvider::new(&model_path);
            match whisper_result {
                Ok(provider) => {
                    state
                        .whisper_loaded
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    tracing::info!(
                        model_path = %model_path.display(),
                        "Whisper local model loaded"
                    );

                    // Share provider via Arc so a warmup task can run alongside the
                    // streaming pipeline. The first inference call on the Vulkan
                    // backend pays a ~6 s shader-pipeline init; we eat that cost
                    // here on a blocking pool thread so the first real utterance
                    // hits a warm pipeline at ~0.15 s.
                    let provider = Arc::new(provider);
                    // Grab the live STT config handle so the UI pickers can mutate it.
                    let live_config = provider.live_config();
                    wire_language_picker_callbacks(ui, live_config.clone());
                    let warmup_provider = provider.clone();
                    state
                        .whisper_warming_up
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    let state_warmup = state.clone();
                    runtime.spawn_blocking(move || {
                        if let Err(e) = warmup_provider.warmup() {
                            tracing::warn!(error = ?e, "Whisper warmup failed (non-fatal)");
                        }
                        state_warmup
                            .whisper_warming_up
                            .store(false, std::sync::atomic::Ordering::Relaxed);
                    });

                    spawn_whisper_pipeline(
                        &runtime,
                        provider,
                        utt_rx,
                        state.clone(),
                        session.as_ref().map(SessionContext::clone_for_task),
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        model_path = %model_path.display(),
                        "Whisper unavailable — falling back to utterance counter only"
                    );
                    spawn_utterance_counter(&runtime, utt_rx, state.clone());
                }
            }
        }
    }

    tracing::info!(
        device = %device_name,
        sample_rate,
        channels,
        whisper = state.whisper_loaded.load(std::sync::atomic::Ordering::Relaxed),
        "audio pipeline wired"
    );

    let icon = if initial_kind == DeviceKind::Output {
        "🔊"
    } else {
        "🎙"
    };
    let kind_label = initial_kind.as_str();
    ui.set_audio_status(
        format!("{icon}  {device_name}  ·  {kind_label}  ·  {sample_rate} Hz  ·  {channels} ch")
            .into(),
    );

    // Wire the FTS5 search callback. Shared `current_query` mutex tracks whether
    // the user is in search mode so the History Timer can skip its refresh and
    // not clobber search results.
    let current_query: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    {
        let ui_weak_search = ui.as_weak();
        let ctx_for_search = session.as_ref().map(SessionContext::clone_for_task);
        let q_for_callback = current_query.clone();
        ui.on_search_changed(move |query| {
            let q = query.to_string();
            if let Ok(mut g) = q_for_callback.lock() {
                *g = q.clone();
            }
            let Some(ref ctx) = ctx_for_search else {
                return;
            };
            let Ok(guard) = ctx.conn.try_lock() else {
                tracing::debug!("search: DB busy, skipping");
                return;
            };
            let text = if q.trim().is_empty() {
                format_recent_sessions(&guard, ctx.session_id, app_started_at)
            } else {
                format_search_results(&guard, q.trim())
            };
            if let Some(ui) = ui_weak_search.upgrade() {
                ui.set_history_status(text.into());
            }
        });
    }

    // Soniox-style transcript stream — Slint VecModel held outside Timer so
    // we can push lines into it from the event consumer indirectly (via the
    // shared PipelineState mutex). The Timer mirrors the Rust Vec into Slint
    // model whenever the length differs (cheap: integer compare per tick).
    let transcript_model: Rc<slint::VecModel<TranscriptLine>> =
        Rc::new(slint::VecModel::from(Vec::<TranscriptLine>::new()));
    ui.set_transcript_lines(slint::ModelRc::from(transcript_model.clone()));

    // Slint timer @ ~30 fps: pull pipeline state into both windows (main + pill).
    // History card is heavier (DB query) — throttled to ~1 Hz via tick counter.
    let ui_weak = ui.as_weak();
    let peak_clone = peak.clone();
    let overflow_clone = overflow.clone();
    let state_ui = state.clone();
    let session_for_timer = session.as_ref().map(SessionContext::clone_for_task);
    let current_query_for_timer = current_query.clone();
    let transcript_model_for_timer = transcript_model.clone();
    let mut ticks: u32 = 0;
    // Track last-seen generation of the transcript stream so we only push
    // to Slint when something actually changed (append or in-place update).
    let mut last_stream_gen: u64 = 0;
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(33),
        move || {
            let level = peak_clone.read_and_reset();
            let dropped = overflow_clone.take_count();
            let count = state_ui.utterance_count.load(Ordering::Relaxed);
            let seq = state_ui.last_seq.load(Ordering::Relaxed);
            let dur_ms = state_ui.last_duration_ms.load(Ordering::Relaxed);
            let whisper_on = state_ui
                .whisper_loaded
                .load(std::sync::atomic::Ordering::Relaxed);
            let warming = state_ui
                .whisper_warming_up
                .load(std::sync::atomic::Ordering::Relaxed);

            let session_id = state_ui.session_id.load(Ordering::Relaxed);
            let persisted = state_ui.segments_persisted.load(Ordering::Relaxed);
            let online = state_ui.provider_online.load(Ordering::Acquire);

            let transcript = state_ui
                .last_transcript
                .lock()
                .map(|s| s.clone())
                .unwrap_or_default();

            // Update main window
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_peak_level(level);
                ui.set_provider_online(online);

                let footer = if warming {
                    "🔥  Đang khởi động Whisper  ·  compile Vulkan shader pipelines …".to_string()
                } else if count == 0 {
                    if whisper_on {
                        "🔇  Listening…  ·  Whisper Base sẵn sàng  ·  Hãy nói gì đó".to_string()
                    } else {
                        "🔇  Listening…  ·  Whisper model not found  ·  utterance counting only"
                            .to_string()
                    }
                } else {
                    format!(
                        "🗣  {count} utterances  ·  last seq {seq}  ·  {:.2}s",
                        dur_ms as f32 / 1000.0
                    )
                };
                ui.set_pipeline_status(footer.into());
                ui.set_last_transcript(transcript.clone().into());

                let storage = if session_id == 0 {
                    "📝  In-memory only — SQLite unavailable".to_string()
                } else {
                    format!("📝  Phiên #{session_id}  ·  {persisted} segments saved (FTS5)")
                };
                ui.set_storage_status(storage.into());

                if dropped > 0 {
                    tracing::warn!(dropped, "ring buffer overflow");
                }
            }

            // Sync Soniox-style transcript stream from Rust Vec → Slint VecModel.
            // Only re-push when the generation counter changed — covers BOTH
            // new lines (Final event) and in-place text updates (Translation
            // event filling pass-B text on an existing row).
            let stream_gen = state_ui.transcript_stream_gen.load(Ordering::Acquire);
            if let Ok(stream) = state_ui.transcript_stream.lock() {
                if stream_gen != last_stream_gen {
                    last_stream_gen = stream_gen;
                    // Truncate + extend. We push all rows because Slint VecModel
                    // doesn't expose a fast "replace from N" — but ≤200 entries so
                    // this is cheap (microseconds).
                    let new_rows: Vec<TranscriptLine> = stream
                        .iter()
                        .map(|l| TranscriptLine {
                            seq: l.seq,
                            time: l.time.clone().into(),
                            lang: l.lang.clone().into(),
                            text: l.text.clone().into(),
                            original_lang: l.original_lang.clone().into(),
                            original_text: l.original_text.clone().into(),
                            duration_ms: l.duration_ms,
                            translation_done: l.translation_done,
                        })
                        .collect();
                    transcript_model_for_timer.set_vec(new_rows);
                }
            }

            // History card: throttle DB query to ~1 Hz. Skip when user is
            // in search mode (non-empty query) so we don't clobber their results.
            ticks = ticks.wrapping_add(1);
            if ticks % 30 == 0 {
                let searching = current_query_for_timer
                    .lock()
                    .map(|g| !g.trim().is_empty())
                    .unwrap_or(false);
                if !searching {
                    if let Some(ref ctx) = session_for_timer {
                        if let Ok(guard) = ctx.conn.try_lock() {
                            let history_text =
                                format_recent_sessions(&guard, ctx.session_id, app_started_at);
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.set_history_status(history_text.into());
                            }
                        }
                    }
                }
            }
        },
    );

    // Wire the hot-swap callback for the audio-source picker.
    // - User picks a device in the ComboBox → callback fires
    // - Build a fresh `AudioSource` for that device
    // - Replace the active source (`RefCell` swap drops the old one — old capture
    //   stops, old resampler abort handle aborts the task, ring buffer freed)
    // - Update audio-status string + persist preference for next launch
    {
        let source_ref = active_source.clone();
        let audio_tx_cb = audio_tx.clone();
        let peak_cb = peak.clone();
        let overflow_cb = overflow.clone();
        let runtime_handle = runtime.handle().clone();
        let ui_weak_cb = ui.as_weak();
        ui.on_audio_source_changed(move |label| {
            let descriptors = list_all_devices().unwrap_or_default();
            let Some(d) = descriptors
                .iter()
                .find(|x| format_device_label(x) == label.as_str())
            else {
                tracing::warn!(label = %label, "audio source label not found");
                return;
            };

            match build_audio_source_from(
                &runtime_handle,
                audio_tx_cb.clone(),
                &peak_cb,
                &overflow_cb,
                &d.name,
                d.kind,
            ) {
                Ok(new_source) => {
                    let new_name = new_source.device_name.clone();
                    let new_rate = new_source.sample_rate;
                    let new_ch = new_source.channels;
                    let new_kind = new_source.kind;
                    // Drop happens here: borrow_mut takes the old Option<AudioSource>,
                    // replaces with Some(new). Old AudioSource Drops → stops stream.
                    let _old = source_ref.borrow_mut().replace(new_source);
                    let _ = write_audio_source_config(&new_name, new_kind);
                    tracing::info!(
                        device = %new_name,
                        kind = new_kind.as_str(),
                        sample_rate = new_rate,
                        channels = new_ch,
                        "audio source hot-swapped"
                    );
                    if let Some(ui) = ui_weak_cb.upgrade() {
                        let icon = if new_kind == DeviceKind::Output {
                            "🔊"
                        } else {
                            "🎙"
                        };
                        ui.set_audio_status(
                            format!(
                                "{icon}  {new_name}  ·  {}  ·  {new_rate} Hz  ·  {new_ch} ch",
                                new_kind.as_str()
                            )
                            .into(),
                        );
                        // Hot-swap succeeded → no restart needed
                        ui.set_restart_needed(false);
                        ui.set_current_audio_source(format_device_label(d).into());
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        device = %d.name,
                        kind = d.kind.as_str(),
                        "failed to hot-swap audio source — keeping previous"
                    );
                    if let Some(ui) = ui_weak_cb.upgrade() {
                        // Surface failure: keep restart-needed false but show error in status
                        ui.set_audio_status(
                            format!("⚠️  Switch failed: {e}  ·  giữ nguyên nguồn cũ").into(),
                        );
                    }
                }
            }
        });
    }

    // Export-to-Markdown callback. Picks the most-recent SQLite session row,
    // formats via `vong_storage::export_markdown`, writes to
    // `~/Documents/Vong/session-{id}-{started_at}.md`. Updates UI status string.
    {
        let session_for_export = session.as_ref().map(SessionContext::clone_for_task);
        let ui_weak_export = ui.as_weak();
        ui.on_export_clicked(move || {
            let status = do_export_latest(&session_for_export);
            if let Some(ui) = ui_weak_export.upgrade() {
                ui.set_export_status(status.into());
            }
        });
    }

    Ok(AudioBundle {
        _active_source: active_source,
        _timer: timer,
        _runtime: runtime,
        session,
    })
}

/// Resolve latest session id, format as Markdown, save to Documents/Vong/.
/// Returns a UI-ready status string (success path includes the saved path).
fn do_export_latest(session: &Option<SessionContext>) -> String {
    let Some(ctx) = session else {
        return "⚠️  Storage unavailable — không có phiên để xuất".to_string();
    };
    let Ok(guard) = ctx.conn.try_lock() else {
        return "⚠️  DB đang bận — thử lại trong 1 giây".to_string();
    };

    let session_id = match list_recent_sessions(&guard, 1) {
        Ok(sessions) if !sessions.is_empty() => sessions[0].id,
        Ok(_) => return "⚠️  Chưa có phiên ghi nào".to_string(),
        Err(e) => {
            tracing::warn!(error = ?e, "list_recent_sessions failed during export");
            return "⚠️  Không truy vấn được danh sách phiên".to_string();
        }
    };

    let md = match export_markdown(&guard, session_id) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = ?e, session_id, "export_markdown failed");
            return format!("⚠️  Xuất thất bại: {e}");
        }
    };
    drop(guard); // release lock before disk I/O

    let docs_dir = directories::UserDirs::new()
        .and_then(|d| d.document_dir().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let target_dir = docs_dir.join("Vong");
    if let Err(e) = std::fs::create_dir_all(&target_dir) {
        tracing::warn!(error = ?e, dir = %target_dir.display(), "failed to create export dir");
        return format!("⚠️  Không tạo được thư mục: {e}");
    }

    let filename = format!("session-{session_id}.md");
    let path = target_dir.join(&filename);
    match std::fs::write(&path, md) {
        Ok(()) => {
            tracing::info!(path = %path.display(), session_id, "exported session to markdown");
            format!("✓  Đã lưu: {}", path.display())
        }
        Err(e) => {
            tracing::warn!(error = ?e, path = %path.display(), "failed to write export file");
            format!("⚠️  Ghi file thất bại: {e}")
        }
    }
}

/// Build an `AudioSource` from the saved config (or default-input fallback).
fn build_audio_source_from_config(
    runtime: &tokio::runtime::Runtime,
    audio_tx: tokio::sync::mpsc::Sender<Vec<i16>>,
    peak: &PeakMeter,
    overflow: &OverflowCounter,
) -> Result<AudioSource, AudioError> {
    let (_, device_info) = resolve_audio_device();
    tracing::info!(
        device = %device_info.name,
        kind = device_info.kind.as_str(),
        is_default = device_info.is_default,
        "audio source resolved"
    );
    build_audio_source_from(
        runtime.handle(),
        audio_tx,
        peak,
        overflow,
        &device_info.name,
        device_info.kind,
    )
}

/// Build an `AudioSource` for a specific (name, kind) pair. Spawns a resampler
/// task on `runtime` that drains the cpal ring into `audio_tx`. Returned
/// `AudioSource` holds the abort handle so dropping it stops everything.
fn build_audio_source_from(
    runtime: &tokio::runtime::Handle,
    audio_tx: tokio::sync::mpsc::Sender<Vec<i16>>,
    peak: &PeakMeter,
    overflow: &OverflowCounter,
    name: &str,
    kind: DeviceKind,
) -> Result<AudioSource, AudioError> {
    let device = find_device(name, kind)?;
    let supported = negotiate_config(&device, kind)?;
    let sample_rate = supported.sample_rate();
    let channels = supported.channels();

    // ~5 s of headroom @ 48 kHz stereo. Resampler drains this on the runtime.
    let (tx, rx) = RingBuffer::<i16>::new(480_000);
    let handle = start_capture(&device, kind, tx, peak.clone(), overflow.clone())?;

    let cfg = ResampleConfig {
        source_rate: sample_rate,
        source_channels: channels.max(1),
        read_chunk_size: 2048,
    };
    let resampler_join = runtime.spawn(async move {
        if let Err(e) = run_resampler(rx, audio_tx, cfg).await {
            tracing::error!(error = ?e, "resampler task exited with error");
        } else {
            tracing::info!("resampler task exited (channel closed — likely source swap)");
        }
    });

    Ok(AudioSource {
        _handle: handle,
        _resampler_abort: resampler_join.abort_handle(),
        sample_rate,
        channels,
        device_name: name.to_string(),
        kind,
    })
}

/// Open SQLite at the platform-default path and insert a fresh `sessions` row.
/// Non-fatal: returns `None` on any failure (logged warning).
fn init_storage(device_name: &str, state: Arc<PipelineState>) -> Option<SessionContext> {
    let conn = match open_default() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = ?e, "storage: open_default failed — persistence disabled");
            return None;
        }
    };
    let path = default_db_path().ok();
    let started_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let session_id = match insert_session(
        &conn,
        &NewSession {
            started_at_ms,
            audio_source: "mic".into(),
            provider: "whisper-local".into(),
            detected_language: Some("vi".into()),
            meta_json: Some(format!(r#"{{"device":"{}"}}"#, sanitize_json(device_name))),
        },
    ) {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = ?e, "storage: insert_session failed — persistence disabled");
            return None;
        }
    };

    tracing::info!(
        session_id,
        db_path = ?path,
        "storage: session opened"
    );
    state.session_id.store(session_id as u64, Ordering::Relaxed);

    Some(SessionContext {
        conn: Arc::new(Mutex::new(conn)),
        session_id,
        started_at_ms,
    })
}

/// Minimal JSON-string sanitizer: escape `"` and `\`. We only embed the device
/// name into a tiny JSON blob, so a full serde round-trip would be overkill.
fn sanitize_json(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// ---- Wizard callbacks (Phase 3) -------------------------------------------

/// Wire all wizard-related Slint callbacks onto `ui`.
///
/// Called once from `main()` after `AppWindow::new()`. All heavy async work
/// (API key test) is dispatched via `std::thread::spawn` + one-shot tokio
/// runtime to avoid blocking the Slint event loop.
fn wire_wizard_callbacks(ui: &AppWindow, _models_dir: &Path) {
    // ── Next ─────────────────────────────────────────────────────────────────
    {
        let ui_weak = ui.as_weak();
        ui.on_wizard_next(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let current = ui.get_wizard_step() as usize;
            let provider = ui.get_wizard_provider().to_string();

            // Persist the step we just completed.
            if let Some(step) = wizard::WizardStep::from_index(current) {
                if let Err(e) = wizard::mark_step_done(step) {
                    tracing::warn!(
                        error = ?e,
                        step = ?step,
                        "wizard: failed to persist step — continuing anyway"
                    );
                }
                tracing::info!(step = current, "wizard step complete");
            }

            let next = wizard::compute_next_step_index(current, &provider);
            ui.set_wizard_step(next as i32);
        });
    }

    // ── Back ─────────────────────────────────────────────────────────────────
    {
        let ui_weak = ui.as_weak();
        ui.on_wizard_back(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let current = ui.get_wizard_step() as usize;
            let provider = ui.get_wizard_provider().to_string();
            // Back does NOT remove lines from onboarded.txt — append-only.
            let prev = wizard::compute_prev_step_index(current, &provider);
            ui.set_wizard_step(prev as i32);
            tracing::info!(from_step = current, to_step = prev, "wizard: back navigation");
        });
    }

    // ── Skip (Step 3 — API key) ──────────────────────────────────────────────
    {
        let ui_weak = ui.as_weak();
        ui.on_wizard_skip(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            if let Err(e) = wizard::mark_api_key_skipped() {
                tracing::warn!(error = ?e, "wizard: failed to persist api_key.skipped");
            }
            tracing::info!("wizard: api key step skipped");
            // ApiKey (3) → Language (5) regardless of provider (skip goes past model too).
            ui.set_wizard_step(5);
        });
    }

    // ── Complete (Step 6 CTA) ────────────────────────────────────────────────
    {
        let ui_weak = ui.as_weak();
        ui.on_wizard_complete(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            if let Err(e) = wizard::mark_step_done(wizard::WizardStep::Done) {
                tracing::warn!(error = ?e, "wizard: failed to persist done step");
            }
            if let Err(e) = wizard::mark_complete() {
                tracing::warn!(error = ?e, "wizard: failed to write step.complete");
            }
            tracing::info!("wizard: complete — switching to main view");
            ui.set_current_view(slint::SharedString::from("main"));
        });
    }

    // ── Test API key ─────────────────────────────────────────────────────────
    {
        let ui_weak = ui.as_weak();
        ui.on_wizard_test_api_key(move |provider, key| {
            let ui_weak = ui_weak.clone();
            let provider = provider.to_string();
            let key = key.to_string();

            // Mark as testing (disables the button immediately).
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_api_key_testing(true);
                ui.set_api_key_test_result(slint::SharedString::from(""));
            }

            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("wizard api key test runtime");
                let result = rt.block_on(wizard::test_api_key(&provider, &key));
                let result_str = match result {
                    Ok(()) => "ok".to_string(),
                    Err(msg) => msg,
                };
                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_api_key_testing(false);
                        ui.set_api_key_test_result(slint::SharedString::from(result_str));
                    }
                })
                .ok();
            });
        });
    }

    // ── Save API key ─────────────────────────────────────────────────────────
    {
        ui.on_wizard_save_api_key(move |provider, key| {
            let provider = provider.to_string();
            let key = key.to_string();
            match wizard::save_api_key(&provider, &key) {
                Ok(()) => tracing::info!(
                    provider = %provider,
                    key_len = key.len(),
                    "wizard: API key stored in Credential Manager"
                ),
                Err(e) => tracing::warn!(
                    provider = %provider,
                    error = %e,
                    "wizard: failed to store API key"
                ),
            }
        });
    }

    // ── Language changes from wizard (update live config immediately) ─────────
    // Forward wizard source/target picks to the same callbacks wired by
    // wire_language_picker_callbacks (Settings screen). Slint generates
    // `invoke_*` for user-defined callbacks.
    {
        let ui_weak = ui.as_weak();
        ui.on_wizard_source_changed(move |lang| {
            if let Some(ui) = ui_weak.upgrade() {
                ui.invoke_source_language_changed(lang);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_wizard_target_changed(move |mode| {
            if let Some(ui) = ui_weak.upgrade() {
                ui.invoke_target_mode_changed(mode);
            }
        });
    }
}

// ---- Provider mode persistence (which STT engine to run) -------------------

/// `%APPDATA%\Vong\Vong AI Recorder\config\provider.txt` — single-line provider id
/// (`local-whisper` | `soniox` | `openai-realtime`).
fn provider_config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "Vong", "Vong AI Recorder")
        .map(|d| d.config_dir().join("provider.txt"))
}

fn read_provider_mode() -> ProviderMode {
    if let Some(path) = provider_config_path() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Some(mode) = ProviderMode::from_id(content.trim()) {
                return mode;
            }
        }
    }
    ProviderMode::default()
}

fn write_provider_mode(mode: ProviderMode) -> std::io::Result<()> {
    let Some(path) = provider_config_path() else {
        return Err(std::io::Error::other("ProjectDirs unavailable"));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, mode.id())
}

// ---- Audio source picker (Phase 2-W: input + output loopback) -------------

/// Persisted-source config path under `%APPDATA%\Vong\Vong AI Recorder\config\source.txt`.
/// Format: a single line `<kind>|<name>` where kind ∈ {input, loopback}.
fn audio_source_config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "Vong", "Vong AI Recorder").map(|d| d.config_dir().join("source.txt"))
}

fn read_audio_source_config() -> Option<(String, DeviceKind)> {
    let path = audio_source_config_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    let line = content.lines().next()?.trim();
    let (kind_str, name) = line.split_once('|')?;
    let kind = kind_str.parse::<DeviceKind>().ok()?;
    Some((name.to_string(), kind))
}

fn write_audio_source_config(name: &str, kind: DeviceKind) -> std::io::Result<()> {
    let Some(path) = audio_source_config_path() else {
        return Err(std::io::Error::other("ProjectDirs unavailable"));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, format!("{}|{}\n", kind.as_str(), name))
}

/// Resolve the effective audio source: saved config → default input fallback.
#[allow(deprecated)]
fn resolve_audio_device() -> (cpal::Device, DeviceInfo) {
    if let Some((name, kind)) = read_audio_source_config() {
        match find_device(&name, kind) {
            Ok(device) => {
                return (
                    device,
                    DeviceInfo {
                        name,
                        kind,
                        is_default: false,
                    },
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    saved_name = %name,
                    saved_kind = kind.as_str(),
                    "saved audio source not found on this host — falling back to default input"
                );
            }
        }
    }
    let device = default_input_device().expect("no default input device");
    let name = device.name().unwrap_or_else(|_| "Unknown".into());
    (
        device,
        DeviceInfo {
            name,
            kind: DeviceKind::Input,
            is_default: true,
        },
    )
}

/// Build a display label `"🎙 Microphone (mặc định)"` / `"🔊 Speakers (loopback)"`.
fn format_device_label(d: &DeviceInfo) -> String {
    let icon = match d.kind {
        DeviceKind::Input => "🎙",
        DeviceKind::Output => "🔊",
    };
    let suffix = match (d.kind, d.is_default) {
        (DeviceKind::Output, _) => " (loopback)",
        (DeviceKind::Input, true) => " (mặc định)",
        (DeviceKind::Input, false) => "",
    };
    format!("{}  {}{}", icon, d.name, suffix)
}

// ─── Provider picker options ───
//
// Labels carry an emoji prefix the Slint side parses to decide whether the
// API-key input is visible (cloud providers only). Keep the 🧠 / ☁ prefix
// when changing labels.

fn provider_label(mode: ProviderMode) -> &'static str {
    match mode {
        ProviderMode::LocalWhisper => "🧠  Whisper local (offline)",
        ProviderMode::SonioxCloud => "☁  Soniox (BYOK, word-streaming)",
        ProviderMode::OpenAIRealtime => "☁  OpenAI Realtime (BYOK, WIP)",
    }
}

fn parse_provider_label(s: &str) -> Option<ProviderMode> {
    if s.contains("Whisper local") {
        Some(ProviderMode::LocalWhisper)
    } else if s.contains("Soniox") {
        Some(ProviderMode::SonioxCloud)
    } else if s.contains("OpenAI") {
        Some(ProviderMode::OpenAIRealtime)
    } else {
        None
    }
}

const PROVIDER_OPTIONS: &[ProviderMode] = &[
    ProviderMode::LocalWhisper,
    ProviderMode::SonioxCloud,
    ProviderMode::OpenAIRealtime,
];

fn populate_provider_picker(ui: &AppWindow) {
    let labels: Vec<slint::SharedString> =
        PROVIDER_OPTIONS.iter().map(|m| provider_label(*m).into()).collect();
    ui.set_provider_mode_options(slint::ModelRc::from(Rc::new(slint::VecModel::from(labels))));
    let current = read_provider_mode();
    ui.set_current_provider_mode(provider_label(current).into());
    ui.set_provider_is_cloud(current != ProviderMode::LocalWhisper);
}

/// Wire the provider picker + API-key save button. Changing the provider
/// writes the config file and flips `provider-restart-required` so the UI
/// tells the user to relaunch. Saving the key calls `ApiKey::store(…)`.
fn wire_provider_picker_callbacks(ui: &AppWindow) {
    let initial = read_provider_mode();
    let initial_label = provider_label(initial).to_string();
    let ui_weak = ui.as_weak();
    ui.on_provider_mode_changed(move |label| {
        let Some(new_mode) = parse_provider_label(label.as_str()) else {
            tracing::warn!(label = %label, "unknown provider label");
            return;
        };
        if let Err(e) = write_provider_mode(new_mode) {
            tracing::warn!(error = ?e, "failed to write provider config");
        } else {
            tracing::info!(provider = %new_mode.id(), "provider mode persisted (restart to apply)");
        }
        if let Some(ui) = ui_weak.upgrade() {
            // Only flag restart-required when the new selection differs from
            // the mode running this session.
            ui.set_provider_restart_required(label.as_str() != initial_label.as_str());
            // Toggle API-key field visibility based on new selection.
            ui.set_provider_is_cloud(new_mode != ProviderMode::LocalWhisper);
        }
    });

    ui.on_save_provider_key(move |provider_label, key| {
        let key_str = key.to_string();
        let Some(mode) = parse_provider_label(provider_label.as_str()) else {
            tracing::warn!(label = %provider_label, "save key: unknown provider label");
            return;
        };
        if key_str.trim().is_empty() {
            tracing::warn!("save key: empty value — ignored");
            return;
        }
        let provider_id = mode.id();
        match ApiKey::store(provider_id, &key_str) {
            Ok(()) => tracing::info!(provider = provider_id, "API key stored in keychain"),
            Err(e) => tracing::warn!(provider = provider_id, error = ?e, "API key store failed"),
        }
    });
}

// ─── Language picker options ───
//
// Static tables mapping UI label → live-config value. The Slint ComboBox
// shows labels and emits the selected label back; Rust looks the label up
// to translate into `Option<String>` (source) or `TargetMode` (target).

/// (label, source_language). `None` = auto-detect.
const SOURCE_LANGUAGE_OPTIONS: &[(&str, Option<&str>)] = &[
    ("🌐  Tự động phát hiện", None),
    ("🇻🇳  Tiếng Việt", Some("vi")),
    ("🇬🇧  English", Some("en")),
    ("🇯🇵  日本語", Some("ja")),
    ("🇰🇷  한국어", Some("ko")),
    ("🇨🇳  中文", Some("zh")),
    ("🇫🇷  Français", Some("fr")),
    ("🇩🇪  Deutsch", Some("de")),
    ("🇪🇸  Español", Some("es")),
];

/// (label, target_mode). Whisper's only true cross-lingual translation
/// target is English — Hint variants for other languages are essentially
/// forced transcription (good for same-source-same-target denoising,
/// produces phonetic transliteration garbage otherwise).
fn target_mode_options() -> Vec<(&'static str, TargetMode)> {
    vec![
        (
            "🇬🇧  Dịch sang English (translate)",
            TargetMode::TranslateToEnglish,
        ),
        (
            "🇻🇳  Phiên âm tiếng Việt (hint)",
            TargetMode::Hint("vi".into()),
        ),
        ("🇯🇵  Phiên âm 日本語 (hint)", TargetMode::Hint("ja".into())),
        ("🇰🇷  Phiên âm 한국어 (hint)", TargetMode::Hint("ko".into())),
        ("🇨🇳  Phiên âm 中文 (hint)", TargetMode::Hint("zh".into())),
        ("❌  Tắt bản dịch", TargetMode::Off),
    ]
}

/// Populate the source-language and target-mode ComboBox models on app
/// startup. Selects the default for each.
fn populate_language_pickers(ui: &AppWindow) {
    let src_labels: Vec<slint::SharedString> = SOURCE_LANGUAGE_OPTIONS
        .iter()
        .map(|(label, _)| (*label).into())
        .collect();
    ui.set_source_language_options(slint::ModelRc::from(Rc::new(slint::VecModel::from(
        src_labels,
    ))));
    // Default: first entry ("Tự động phát hiện")
    ui.set_current_source_language(SOURCE_LANGUAGE_OPTIONS[0].0.into());

    let tgt_labels: Vec<slint::SharedString> = target_mode_options()
        .into_iter()
        .map(|(label, _)| label.into())
        .collect();
    ui.set_target_mode_options(slint::ModelRc::from(Rc::new(slint::VecModel::from(
        tgt_labels,
    ))));
    // Default: first entry ("Dịch sang English (translate)")
    ui.set_current_target_mode(target_mode_options()[0].0.into());
}

/// Wire the source/target language ComboBox callbacks to mutate
/// `LiveSttConfig`. The next utterance Whisper processes will use the
/// new values — no stream restart needed.
fn wire_language_picker_callbacks(ui: &AppWindow, live_config: LiveConfigHandle) {
    let cfg_src = live_config.clone();
    ui.on_source_language_changed(move |label| {
        let lookup = SOURCE_LANGUAGE_OPTIONS
            .iter()
            .find(|(l, _)| *l == label.as_str());
        let new_value = lookup.and_then(|(_, code)| code.map(String::from));
        if let Ok(mut g) = cfg_src.lock() {
            g.source_language = new_value.clone();
        }
        tracing::info!(source_language = ?new_value, "source language updated");
    });

    let cfg_tgt = live_config;
    ui.on_target_mode_changed(move |label| {
        let modes = target_mode_options();
        let new_mode = modes
            .into_iter()
            .find(|(l, _)| *l == label.as_str())
            .map(|(_, m)| m)
            .unwrap_or(TargetMode::TranslateToEnglish);
        if let Ok(mut g) = cfg_tgt.lock() {
            g.target_mode = new_mode.clone();
        }
        tracing::info!(target_mode = ?new_mode, "target mode updated");
    });
}

fn populate_audio_source_picker(ui: &AppWindow) {
    let descriptors = list_all_devices().unwrap_or_default();
    tracing::info!(
        count = descriptors.len(),
        "audio source enumeration complete"
    );
    for d in &descriptors {
        tracing::info!(
            name = %d.name,
            kind = d.kind.as_str(),
            is_default = d.is_default,
            "  · device"
        );
    }
    let labels: Vec<slint::SharedString> = descriptors
        .iter()
        .map(|d| format_device_label(d).into())
        .collect();
    let model = Rc::new(slint::VecModel::from(labels));
    ui.set_audio_sources(slint::ModelRc::from(model));

    let (_, current_info) = resolve_audio_device();
    ui.set_current_audio_source(format_device_label(&current_info).into());

    // NB: the `audio_source_changed` callback is wired inside `init_audio`
    // — it does both the hot-swap and the config persistence in one place.
}

/// Format the 5 most recent sessions as a multi-line UI string.
/// Current session (`current_id`) is annotated with elapsed wall-clock time.
fn format_recent_sessions(
    conn: &Connection,
    current_id: i64,
    app_started_at: std::time::Instant,
) -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    match list_recent_sessions(conn, 5) {
        Ok(sessions) if sessions.is_empty() => "(chưa có phiên nào)".to_string(),
        Ok(sessions) => sessions
            .iter()
            .map(|s| format_session_line(s, current_id, now_ms, app_started_at))
            .collect::<Vec<_>>()
            .join("\n"),
        Err(e) => {
            tracing::warn!(error = ?e, "list_recent_sessions failed");
            "(không lấy được lịch sử)".to_string()
        }
    }
}

fn format_session_line(
    s: &Session,
    current_id: i64,
    now_ms: i64,
    app_started_at: std::time::Instant,
) -> String {
    let lang = s.detected_language.as_deref().unwrap_or("?");
    if s.id == current_id {
        let elapsed = app_started_at.elapsed().as_secs();
        format!(
            "  •  #{} đang ghi  ·  {}  ·  {}",
            s.id,
            lang,
            format_duration_secs(elapsed)
        )
    } else {
        let ago = format_ago_ms(now_ms - s.started_at_ms);
        let dur = s
            .duration_ms
            .map(|d| format_duration_secs((d / 1000) as u64))
            .unwrap_or_else(|| "?".to_string());
        format!("  •  #{} {} trước  ·  {}  ·  {}", s.id, ago, lang, dur)
    }
}

fn format_ago_ms(ms: i64) -> String {
    let sec = ms.max(0) / 1000;
    if sec < 60 {
        format!("{}s", sec)
    } else if sec < 3600 {
        format!("{}m", sec / 60)
    } else if sec < 86_400 {
        format!("{}h{}m", sec / 3600, (sec % 3600) / 60)
    } else {
        format!("{}d", sec / 86_400)
    }
}

fn format_duration_secs(sec: u64) -> String {
    let m = sec / 60;
    let s = sec % 60;
    if m > 0 {
        format!("{}m {}s", m, s)
    } else {
        format!("{}s", s)
    }
}

/// Run an FTS5 search and format hits as a multi-line UI string.
/// The FTS5 tokenizer uses `unicode61 remove_diacritics 2`, so the query
/// "viet" matches "Việt", "viết", "việc" — a key selling point.
fn format_search_results(conn: &Connection, query: &str) -> String {
    match search_transcripts(conn, query, 8) {
        Ok(hits) if hits.is_empty() => format!("  (Không tìm thấy kết quả cho '{}')", query),
        Ok(hits) => {
            let header = format!("  🔎 {} kết quả khớp '{}'", hits.len(), query);
            let rows = hits
                .iter()
                .map(format_search_hit_line)
                .collect::<Vec<_>>()
                .join("\n");
            format!("{}\n{}", header, rows)
        }
        Err(e) => {
            tracing::warn!(error = ?e, query, "search_transcripts failed");
            format!("  (lỗi tìm: {})", e)
        }
    }
}

fn format_search_hit_line(h: &SearchHit) -> String {
    // SQLite's FTS5 snippet() wraps matches in <b>…</b>. We strip the markup
    // since Slint Text doesn't render HTML — the underlying text is enough.
    let snippet = h.snippet.replace("<b>", "").replace("</b>", "");
    let lang = h.language.as_deref().unwrap_or("?");
    format!("  •  #{} · {} · {}", h.session_id, lang, snippet.trim())
}

/// Default `StreamOpts` used by every provider. Per-utterance source language
/// and target mode actually come from `LiveSttConfig` (mutated by UI), but
/// `language_hint` here seeds the live config the first time the provider
/// runs and is also what cloud providers like Soniox use as a startup hint.
fn default_stream_opts() -> StreamOpts {
    StreamOpts {
        language_hint: Some("vi".into()),
        enable_lid: false,
        enable_diarization: false,
        enable_translation_to: None,
    }
}

/// Spawn the streaming-provider driver task. Generic over any
/// `StreamingTranscriber` implementation so we can pick between
/// `WhisperLocalProvider`, `SonioxProvider`, and (future) OpenAI Realtime
/// from the UI provider picker without code duplication.
fn spawn_provider_task(
    runtime: &tokio::runtime::Runtime,
    provider: Arc<dyn StreamingTranscriber + Send + Sync + 'static>,
    utt_rx: tokio::sync::mpsc::Receiver<Utterance>,
    event_tx: tokio::sync::mpsc::Sender<TranscriptEvent>,
    opts: StreamOpts,
) {
    let provider_name = provider.name();
    runtime.spawn(async move {
        if let Err(e) = provider.transcribe_stream(utt_rx, event_tx, opts).await {
            tracing::error!(provider = provider_name, error = ?e, "STT stream exited with error");
        } else {
            tracing::info!(provider = provider_name, "STT stream exited cleanly");
        }
    });
}

/// Wire the Whisper provider into the pipeline (driver task + event consumer
/// that updates `PipelineState` and persists `Final`/`Translation` events).
///
/// `provider` is `Arc` so the caller can share another reference with a parallel
/// warmup task that pre-pays the Vulkan first-call shader-init cost.
fn spawn_whisper_pipeline(
    runtime: &tokio::runtime::Runtime,
    provider: Arc<WhisperLocalProvider>,
    utt_rx: tokio::sync::mpsc::Receiver<Utterance>,
    state: Arc<PipelineState>,
    session: Option<SessionContext>,
) {
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<TranscriptEvent>(64);
    spawn_provider_task(
        runtime,
        provider as Arc<dyn StreamingTranscriber + Send + Sync + 'static>,
        utt_rx,
        event_tx,
        default_stream_opts(),
    );
    spawn_event_consumer(runtime, event_rx, state, session);
}

/// Same as `spawn_whisper_pipeline` but for the Soniox cloud provider.
fn spawn_soniox_pipeline(
    runtime: &tokio::runtime::Runtime,
    provider: Arc<SonioxProvider>,
    utt_rx: tokio::sync::mpsc::Receiver<Utterance>,
    state: Arc<PipelineState>,
    session: Option<SessionContext>,
) {
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<TranscriptEvent>(64);
    spawn_provider_task(
        runtime,
        provider as Arc<dyn StreamingTranscriber + Send + Sync + 'static>,
        utt_rx,
        event_tx,
        default_stream_opts(),
    );
    spawn_event_consumer(runtime, event_rx, state, session);
}

/// Same as `spawn_whisper_pipeline` but for the OpenAI Realtime cloud provider.
fn spawn_openai_pipeline(
    runtime: &tokio::runtime::Runtime,
    provider: Arc<OpenAIRealtimeProvider>,
    utt_rx: tokio::sync::mpsc::Receiver<Utterance>,
    state: Arc<PipelineState>,
    session: Option<SessionContext>,
) {
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<TranscriptEvent>(64);
    spawn_provider_task(
        runtime,
        provider as Arc<dyn StreamingTranscriber + Send + Sync + 'static>,
        utt_rx,
        event_tx,
        default_stream_opts(),
    );
    spawn_event_consumer(runtime, event_rx, state, session);
}

/// Generic event consumer — handles Partial / Final / Translation / Error /
/// Disconnected across all providers. Same semantics regardless of source.
fn spawn_event_consumer(
    runtime: &tokio::runtime::Runtime,
    mut event_rx: tokio::sync::mpsc::Receiver<TranscriptEvent>,
    state: Arc<PipelineState>,
    session: Option<SessionContext>,
) {
    runtime.spawn(async move {
        while let Some(evt) = event_rx.recv().await {
            match evt {
                TranscriptEvent::Connected => {
                    tracing::info!("STT: connected");
                    state
                        .provider_online
                        .store(true, std::sync::atomic::Ordering::Release);
                }
                TranscriptEvent::Partial { seq, text, language } => {
                    // Streaming partial — Whisper's first pass on the in-progress
                    // VAD buffer. We upsert the row so the "Bản gốc" column
                    // updates in real time; "Bản phiên âm" stays in placeholder
                    // until the Final/Translation arrive after VAD packs.
                    upsert_line_for_seq(
                        &state,
                        seq,
                        /* duration_ms */ 0,
                        |line| {
                            line.original_text = text.clone();
                            line.original_lang = language
                                .clone()
                                .unwrap_or_else(|| "?".into());
                        },
                    );
                    if !text.trim().is_empty() {
                        if let Ok(mut g) = state.last_transcript.lock() {
                            *g = text.clone();
                        }
                        if let Ok(mut g) = state.last_language.lock() {
                            *g = language.clone().unwrap_or_default();
                        }
                    }
                }
                TranscriptEvent::Final {
                    seq,
                    text: _,
                    language: _,
                    original_text,
                    original_language,
                    end,
                    ..
                } => {
                    // Pass A final — counts as one utterance + overrides any
                    // partial-derived original_text on the existing row (or
                    // creates the row if no partial arrived for this seq).
                    state.utterance_count.fetch_add(1, Ordering::Relaxed);
                    state.last_seq.store(seq, Ordering::Relaxed);
                    state
                        .last_duration_ms
                        .store(end.as_millis() as u32, Ordering::Relaxed);

                    if !original_text.trim().is_empty() {
                        if let Ok(mut g) = state.last_transcript.lock() {
                            *g = original_text.clone();
                        }
                        if let Ok(mut g) = state.last_language.lock() {
                            *g = original_language.clone().unwrap_or_default();
                        }
                    }

                    upsert_line_for_seq(
                        &state,
                        seq,
                        end.as_millis() as i32,
                        |line| {
                            line.original_text = original_text.clone();
                            line.original_lang = original_language
                                .clone()
                                .unwrap_or_else(|| "?".into());
                            line.duration_ms = end.as_millis() as i32;
                        },
                    );
                }
                TranscriptEvent::Translation {
                    seq,
                    text,
                    target_lang,
                    is_final,
                    ..
                } if is_final => {
                    // Pass B done — fill in the "Bản phiên âm" cell on the
                    // existing row. If no row exists (rare race: Translation
                    // arrived before Final/Partial), create a placeholder one.
                    upsert_line_for_seq(&state, seq, 0, |line| {
                        line.text = text.clone();
                        line.lang = if target_lang.is_empty() {
                            "?".into()
                        } else {
                            target_lang.clone()
                        };
                        line.translation_done = true;
                    });

                    if !text.trim().is_empty() {
                        if let Ok(mut g) = state.last_transcript.lock() {
                            *g = text.clone();
                        }
                        if let Ok(mut g) = state.last_language.lock() {
                            *g = target_lang.clone();
                        }
                    }

                    if let Some(ref ctx) = session {
                        let lang_opt = if target_lang.is_empty() {
                            None
                        } else {
                            Some(target_lang.as_str())
                        };
                        persist_segment(
                            ctx,
                            seq,
                            Duration::ZERO,
                            Duration::ZERO,
                            &text,
                            lang_opt,
                            &state,
                        );
                    }
                }
                TranscriptEvent::Error { code, message } => {
                    tracing::warn!(code, message, "STT: recoverable error");
                }
                TranscriptEvent::Disconnected => {
                    tracing::info!("STT: disconnected");
                    state
                        .provider_online
                        .store(false, std::sync::atomic::Ordering::Release);
                }
                _ => {}
            }
        }
        tracing::info!("STT event consumer exited (channel closed)");
    });
}

/// Find the row for `seq` in the transcript stream and apply `f` to it;
/// if no such row exists, push a new placeholder row and apply `f`. Bumps
/// the generation counter so the Slint Timer re-pushes the model on next tick.
///
/// Caps the stream at 200 rows after any append, dropping the oldest.
fn upsert_line_for_seq(
    state: &PipelineState,
    seq: u64,
    initial_duration_ms: i32,
    f: impl FnOnce(&mut TranscriptStreamLine),
) {
    let wall_time = {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // ICT (UTC+7) wall clock
        let local = (now + 7 * 3600) % 86400;
        let hh = local / 3600;
        let mm = (local % 3600) / 60;
        format!("{:02}:{:02}", hh, mm)
    };
    if let Ok(mut s) = state.transcript_stream.lock() {
        if let Some(line) = s.iter_mut().rev().find(|l| l.seq == seq as i32) {
            f(line);
        } else {
            let mut line = TranscriptStreamLine {
                seq: seq as i32,
                time: wall_time,
                lang: String::new(),
                text: String::new(),
                original_lang: "?".into(),
                original_text: String::new(),
                duration_ms: initial_duration_ms,
                translation_done: false,
            };
            f(&mut line);
            s.push(line);
            if s.len() > 200 {
                let drop_n = s.len() - 200;
                s.drain(..drop_n);
            }
        }
    }
    state.transcript_stream_gen.fetch_add(1, Ordering::Release);
}

/// Insert a `transcript_segments` row. Logs + counts on success; logs warn on
/// failure (best-effort persistence — never crash the pipeline on DB error).
fn persist_segment(
    ctx: &SessionContext,
    seq: u64,
    start: Duration,
    end: Duration,
    text: &str,
    language: Option<&str>,
    state: &PipelineState,
) {
    let row = NewSegment {
        session_id: ctx.session_id,
        seq: seq as i64,
        start_ms: start.as_millis() as i64,
        end_ms: end.as_millis() as i64,
        speaker_label: None,
        text: text.to_string(),
        language: language.map(|s| s.to_string()),
        confidence: None,
        is_final: true,
    };
    match ctx.conn.lock() {
        Ok(guard) => match insert_segment(&guard, &row) {
            Ok(id) => {
                state.segments_persisted.fetch_add(1, Ordering::Relaxed);
                tracing::info!(
                    segment_id = id,
                    session_id = ctx.session_id,
                    seq,
                    "segment persisted"
                );
            }
            Err(e) => tracing::warn!(error = ?e, seq, "insert_segment failed"),
        },
        Err(_) => tracing::warn!(seq, "DB mutex poisoned — segment dropped"),
    }
}

/// Fallback when Whisper is unavailable — just count utterances so the UI
/// shows liveness, no transcription happens.
fn spawn_utterance_counter(
    runtime: &tokio::runtime::Runtime,
    mut utt_rx: tokio::sync::mpsc::Receiver<Utterance>,
    state: Arc<PipelineState>,
) {
    runtime.spawn(async move {
        while let Some(utt) = utt_rx.recv().await {
            state.utterance_count.fetch_add(1, Ordering::Relaxed);
            state.last_seq.store(utt.seq, Ordering::Relaxed);
            state
                .last_duration_ms
                .store(utt.duration_ms, Ordering::Relaxed);
        }
        tracing::info!("utterance counter exited (channel closed)");
    });
}

// ── Phase 2: model download helpers ───────────────────────────────────────

/// Return the models directory where Whisper GGML files are stored.
///
/// Matches case 3 of `WhisperLocalProvider::resolve_default_model_path()`:
/// `%LOCALAPPDATA%\Vong\Vong AI Recorder\data\models\`
fn resolve_models_dir() -> PathBuf {
    if let Some(dirs) = directories::ProjectDirs::from("com", "Vong", "Vong AI Recorder") {
        return dirs.data_local_dir().join("models");
    }
    PathBuf::from("models")
}

/// Wire the `on_start_model_download` / `on_cancel_model_download` callbacks
/// on the Slint `AppWindow`. Phase 3 wizard calls these from its UI.
///
/// The download runs on the existing tokio runtime embedded in `AudioBundle`.
/// Since the runtime may not exist when there's no audio (edge case), we
/// spin up a lightweight one-shot tokio task via `tokio::runtime::Handle`
/// captured from the audio bundle's runtime — but `wire_model_download_callbacks`
/// is called before `init_audio` returns the bundle so we need a standalone
/// approach. We use `std::thread::spawn` + a fresh one-shot tokio runtime
/// so the download doesn't block the Slint event loop.
fn wire_model_download_callbacks(
    ui: &AppWindow,
    dl_progress: Arc<Mutex<DlProgress>>,
    dl_cancel: Arc<std::sync::atomic::AtomicBool>,
    models_dir: &Path,
) {
    let models_dir = models_dir.to_path_buf();
    let dl_cancel_for_start = dl_cancel.clone();
    let dl_progress_for_start = dl_progress.clone();

    ui.on_start_model_download(move |name| {
        let name = name.to_string();
        let progress = dl_progress_for_start.clone();
        let cancel = dl_cancel_for_start.clone();
        let dir = models_dir.clone();

        // Reset cancel flag for a fresh attempt.
        cancel.store(false, std::sync::atomic::Ordering::Relaxed);

        // Spawn on a background thread with its own minimal tokio runtime so
        // the download doesn't block the Slint event loop.
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("download runtime");
            rt.block_on(async move {
                match model_dl::download_model(&name, &dir, progress.clone(), cancel).await {
                    Ok(_) => {
                        tracing::info!(model_name = %name, state_transition = "done",
                            "model download complete");
                    }
                    Err(DlError::Cancelled) => {
                        tracing::info!(model_name = %name, state_transition = "cancelled",
                            "model download cancelled");
                    }
                    Err(e) => {
                        let msg = e.user_message_vi();
                        tracing::warn!(model_name = %name, state_transition = "error",
                            "model download failed");
                        if let Ok(mut p) = progress.lock() {
                            p.state = DownloadState::Error;
                            p.error_msg = msg;
                        }
                    }
                }
            });
        });
    });

    ui.on_cancel_model_download(move || {
        dl_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        tracing::info!(state_transition = "cancel_requested", "model download cancel requested");
    });
}

/// Public entry point for Phase 3 wizard to trigger a model download
/// programmatically (e.g. from the "Download" button in `ModelDownloadStep`).
///
/// Wraps `model_dl::download_model` with the shared progress + cancel handles
/// already wired to the Slint timer. Phase 3 calls this after obtaining
/// the `dl_progress` / `dl_cancel` handles from the app state it receives.
pub fn start_model_download(
    name: &str,
    models_dir: &std::path::Path,
    progress: Arc<Mutex<DlProgress>>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) {
    let name = name.to_string();
    let dir = models_dir.to_path_buf();

    cancel.store(false, std::sync::atomic::Ordering::Relaxed);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("download runtime");
        rt.block_on(async move {
            let _ = model_dl::download_model(&name, &dir, progress, cancel).await;
        });
    });
}
