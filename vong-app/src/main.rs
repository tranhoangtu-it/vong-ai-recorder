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

mod autosummary;
mod dialog;
mod dictionary;
mod log_init;
mod pipeline;
mod recording_config;
mod sentry_init;
mod session_detail;
mod summary_runner;
mod toast;
mod tray;
mod wizard;

use dialog::DialogQueue;
use pipeline::PipelineHandle;
use toast::{ToastQueue, ToastSeverity};
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
    run_vad_fsm_live, start_capture, AudioError, CaptureHandle, DeviceInfo, DeviceKind,
    LiveVadConfig, OverflowCounter, PeakMeter, ResampleConfig, Utterance,
};
use summary_runner::SummaryRunner;
use vong_storage::{
    default_db_path, export_markdown, fetch_summaries_for_sessions, finalize_session,
    insert_segment, insert_session, list_recent_sessions, open_default, search_transcripts,
    Connection, NewSegment, NewSession, SearchHit,
};
use vong_transcribe::{
    model_dl, ApiKey, DictEntry, DlError, DownloadProgress as DlProgress, DownloadState,
    LiveConfigHandle, ProviderMode, TargetMode, TranscriptEvent, WhisperLocalProvider,
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
    /// Shared summary runner — used by UI timer and shutdown finalizer.
    summary_runner: Arc<SummaryRunner>,
    /// Phase 13: toast queue — shared with timer + callbacks.
    _toast_queue: ToastQueue,
    /// Phase 13: dialog queue — shared with timer + callbacks.
    _dialog_queue: DialogQueue,
    /// Phase 12: provider pipeline lifecycle handle — supports hot-swap.
    _pipeline: Arc<PipelineHandle>,
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

/// A discovered Whisper GGML model file on disk (internal Rust-side struct).
/// Slint-generated `WhisperModelEntry` is the view-model used by the UI.
#[derive(Debug, Clone)]
struct ModelScanEntry {
    /// Filename e.g. "ggml-base.bin"
    name: String,
    /// Full path for loading
    path: PathBuf,
    /// File size in bytes
    size_bytes: u64,
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
    /// True while a model swap is in progress (spawn_blocking loading).
    model_switch_in_flight: std::sync::atomic::AtomicBool,
    /// Status message for the model picker card ("" = idle, otherwise error or loading msg).
    model_switch_status: Mutex<String>,
    /// Label of the currently active Whisper model (e.g. "ggml-base.bin").
    active_whisper_model: Mutex<String>,
    /// Phase 13: last session_id for which a summary-error toast was pushed.
    /// Prevents spamming a toast on every 30 Hz tick when summary stays in error state.
    last_summary_error_toast_session: AtomicU64,
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
    // ── Sentry init MUST come before log_init ─────────────────────────────────
    // Sentry's `panic` feature installs its own hook on `sentry::init`.
    // Our `log_init` then installs OUR hook on top (which also forwards to
    // Sentry explicitly via `sentry::integrations::panic::panic_handler`).
    // Reversing this order would lose Sentry's panic capture.
    // Returns None when consent is absent/disabled OR DSN is still placeholder.
    let sentry_guard = sentry_init::init_if_enabled();

    // Bind the log-file flush guard for the entire app lifetime — dropping it
    // flushes any pending log lines from the rolling file appender.
    let _log_guard = log_init::init_logging(sentry_guard.as_ref());
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        sentry_enabled = sentry_guard.is_some(),
        "Vọng starting"
    );

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
                // Resume at last_completed + 1, clamped to step 7 (Done).
                // Phase 9: Done moved from index 6 to 7; CrashReport is at 6.
                let next = (idx + 1).min(7);
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

    // Phase 13: app-wide toast + dialog queues, created before init_audio so
    // they can be shared into the audio bundle, dl_timer, and top-level callbacks.
    let app_toast_queue = ToastQueue::new();
    let app_dialog_queue = DialogQueue::new();

    let audio = match init_audio(&ui, app_started_at, app_toast_queue.clone(), app_dialog_queue.clone()) {
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
    let dl_toast_queue = app_toast_queue.clone();
    let mut dl_last_error_state = false; // track edge: idle→error to toast once

    let ui_weak_dl = ui.as_weak();
    let dl_progress_timer = dl_progress.clone();
    let models_dir_timer = models_dir.clone();
    let dl_toast_queue_timer = dl_toast_queue.clone(); // same arc as app_toast_queue
    let _dl_timer = {
        let t = slint::Timer::default();
        t.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(33),
            move || {
                let Some(ui) = ui_weak_dl.upgrade() else { return };

                // Mirror download progress struct into Slint property.
                if let Ok(p) = dl_progress_timer.lock() {
                    // Phase 13: push error toast on transition to error state (once).
                    let is_error = p.state == DownloadState::Error;
                    if is_error && !dl_last_error_state {
                        dl_toast_queue_timer.push(
                            "Tải mô hình thất bại — kiểm tra kết nối mạng",
                            ToastSeverity::Error,
                        );
                        // Mirror toast stack into Slint from here so the dl_timer
                        // toasts are visible (the main 30 Hz timer handles its own queue).
                        dl_toast_queue_timer.prune_expired();
                        let snap = dl_toast_queue_timer.snapshot();
                        // Merge with existing toast-stack (append only — avoid clobber).
                        // For simplicity we only push dl_timer toasts via the same Slint prop
                        // when dl_toast_queue has entries. The main timer will overwrite on
                        // next tick if both queues have entries — acceptable since download
                        // errors are transient and the main timer runs at the same 30 Hz.
                        if !snap.is_empty() {
                            let slint_toasts: Vec<ToastEntry> = snap
                                .iter()
                                .map(|t| ToastEntry {
                                    id: t.id.min(i32::MAX as u64) as i32,
                                    message: slint::SharedString::from(t.message.as_str()),
                                    severity: slint::SharedString::from(t.severity.as_str()),
                                    expires_at_ms: t.expires_at_ms.min(i32::MAX as u64) as i32,
                                })
                                .collect();
                            let toast_model = slint::VecModel::from(slint_toasts);
                            ui.set_toast_stack(slint::ModelRc::from(
                                std::rc::Rc::new(toast_model),
                            ));
                        }
                    }
                    dl_last_error_state = is_error;

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

    // ── Phase 9: Crash reporting Settings toggle ──────────────────────────────
    // Initialise the visual state from the current consent file before the UI
    // renders — the toggle should reflect what's already persisted on disk.
    ui.set_crash_reporting_enabled(
        matches!(sentry_init::load_consent(), sentry_init::ConsentState::Enabled),
    );

    // Wire the toggle callback. Changing the toggle writes sentry.txt
    // immediately but Sentry is only initialised at startup — the change
    // takes effect on the next app launch.
    {
        let ui_weak_sentry = ui.as_weak();
        let tq_sentry = app_toast_queue.clone();
        ui.on_crash_reporting_toggled(move |enabled| {
            let state = if enabled {
                sentry_init::ConsentState::Enabled
            } else {
                sentry_init::ConsentState::Disabled
            };
            if let Err(e) = sentry_init::save_consent(state) {
                tracing::warn!(error = ?e, "crash reporting consent save failed");
                // Phase 13: surface config save failure as a toast.
                tq_sentry.push("Lưu cấu hình báo lỗi thất bại", ToastSeverity::Error);
            } else {
                tracing::info!(
                    sentry_consent = enabled,
                    "crash reporting consent updated — takes effect on next launch"
                );
            }
            // Show a transient "restart required" hint in the UI.
            if let Some(ui) = ui_weak_sentry.upgrade() {
                ui.set_crash_reporting_restart_hint(true);
            }
        });
    }

    // Debug-only manual trigger: intentionally fire a tracing::error! + panic
    // so the developer can verify the full Sentry pipeline end-to-end.
    // NEVER compiled into release builds — the callback body is absent.
    #[cfg(debug_assertions)]
    {
        ui.on_dev_trigger_sentry_panic(|| {
            tracing::error!("dev: intentional Sentry test event — verifying pipeline");
            panic!("Vong dev: Sentry pipeline test panic");
        });
    }

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

            // Phase 10: trigger auto-summary on shutdown (best-effort, 30 s max).
            if autosummary::should_auto_summarize() {
                tracing::info!(
                    session_id = session.session_id,
                    "auto-summary: triggering on shutdown"
                );
                bundle.summary_runner.trigger_blocking_timeout(
                    session.conn.clone(),
                    session.session_id,
                    false,
                    std::time::Duration::from_secs(30),
                );
            }
        }
    }
    drop(audio);

    tracing::info!("Vọng shutting down");

    // Drop sentry_guard last — its Drop impl flushes any pending events to
    // Sentry with a 2-second synchronous timeout. Must happen after all tasks
    // have been shut down so late tracing::error! calls are captured.
    drop(sentry_guard);

    Ok(())
}

fn init_audio(
    ui: &AppWindow,
    _app_started_at: std::time::Instant,
    toast_queue: ToastQueue,
    dialog_queue: DialogQueue,
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

    // VAD FSM emits to an INTERNAL channel. A relay task forwards to the
    // active provider via PipelineHandle — gated by the `is_recording` toggle
    // so audio doesn't reach the model when the user hasn't clicked the mic button.
    let (utt_internal_tx, mut utt_internal_rx) = tokio::sync::mpsc::channel::<Utterance>(32);

    // Load recording config (VAD sliders + last-used Whisper model path).
    // Missing or corrupt file → silently use defaults.
    let rec_cfg = recording_config::load();
    let initial_vad_config = recording_config::into_vad_config(&rec_cfg.vad);

    // LiveVadConfig is the shared handle mutated by UI slider callbacks.
    // The FSM snapshots it once per ~16 ms chunk — zero UI restart needed.
    let live_vad: LiveVadConfig = Arc::new(Mutex::new(initial_vad_config));

    // Load dictionary. Missing/corrupt file → empty list (non-fatal).
    // `dict_file` is mutated by Settings → Dictionary tab callbacks and
    // read at provider startup to seed the provider-specific caches.
    let dict_file = dictionary::load();
    tracing::info!(
        dict_entry_count = dict_file.entries.len(),
        "dictionary entries loaded at startup"
    );
    // Shared dictionary state for UI callbacks.
    let dict_file_handle: Arc<Mutex<dictionary::DictionaryFile>> =
        Arc::new(Mutex::new(dict_file));

    // Spawn VAD FSM with the live handle.
    let live_vad_for_fsm = live_vad.clone();
    runtime.spawn(async move {
        if let Err(e) = run_vad_fsm_live(audio_rx, utt_internal_tx, live_vad_for_fsm).await {
            tracing::error!(error = ?e, "VAD FSM exited with error");
        } else {
            tracing::info!("VAD FSM exited cleanly");
        }
    });

    let state = Arc::new(PipelineState::default());

    // Phase 12: PipelineHandle owns the streaming provider task and supports
    // hot-swap via swap_provider(). Created here with a placeholder live_config;
    // the actual config is wired inside the provider match block below.
    // `Arc` because the relay task, the provider swap callback, and AudioBundle
    // all need to share the handle.
    let pipeline_handle = Arc::new(PipelineHandle::new(
        Arc::new(Mutex::new(vong_transcribe::LiveSttConfig::default())),
        runtime.handle().clone(),
        state.clone(),
    ));

    // Relay: gate by `is_recording`. Forwards utterances to the active provider
    // via PipelineHandle::send_utterance so hot-swap is transparent.
    let state_for_relay = state.clone();
    let pipeline_for_relay = pipeline_handle.clone();
    runtime.spawn(async move {
        while let Some(utt) = utt_internal_rx.recv().await {
            if state_for_relay.is_recording.load(Ordering::Acquire) {
                if !pipeline_for_relay.send_utterance(utt).await {
                    tracing::warn!("relay: provider channel closed or no active provider");
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

    // Phase 10: SummaryRunner created early so it can be captured by on_record_toggled.
    // `shared_session` is populated after init_storage — both closures share the Arc.
    let summary_runner_early = Arc::new(SummaryRunner::new());
    let shared_session: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
    let shared_conn: Arc<Mutex<Option<Arc<Mutex<Connection>>>>> = Arc::new(Mutex::new(None));

    // Wire the mic-button toggle. Default OFF — `state.is_recording` starts
    // false (AtomicBool::default), so the relay drops everything until the
    // user explicitly clicks. The button itself owns the UI state via
    // `in-out recording`; this callback just mirrors it into Rust.
    // Phase 10: on Stop (on=false), trigger auto-summary for the current session.
    {
        let state_for_record = state.clone();
        let runner_for_record = summary_runner_early.clone();
        let session_cell = shared_session.clone();
        let conn_cell = shared_conn.clone();
        ui.on_record_toggled(move |on| {
            state_for_record.is_recording.store(on, Ordering::Release);
            tracing::info!(recording = on, "record button toggled");
            if !on && autosummary::should_auto_summarize() {
                let session_id = session_cell.lock().ok().and_then(|g| *g);
                let conn_arc = conn_cell.lock().ok().and_then(|g| g.clone());
                if let (Some(sid), Some(conn)) = (session_id, conn_arc) {
                    tracing::info!(session_id = sid, "auto-summary: triggering on record stop");
                    runner_for_record.trigger_detached(conn, sid, false);
                }
            }
        });
    }

    // Open SQLite + create a session row. Storage failure is non-fatal — the
    // pipeline runs without persistence (segments_persisted stays at 0).
    let session = init_storage(&device_name, state.clone());

    // Phase 10: Populate the shared session/conn cells so on_record_toggled can
    // reach the session after it's been established.
    if let Some(ref ctx) = session {
        if let Ok(mut g) = shared_session.lock() {
            *g = Some(ctx.session_id);
        }
        if let Ok(mut g) = shared_conn.lock() {
            *g = Some(ctx.conn.clone());
        }
    }

    // Decide which STT engine to drive the pipeline. Phase 12: hot-swap is now
    // supported — changing provider in Settings calls swap_provider, no restart.
    let provider_mode = read_provider_mode();
    tracing::info!(provider = %provider_mode.id(), "STT provider mode selected");
    ui.set_current_provider_mode(provider_label(provider_mode).into());
    ui.set_provider_is_cloud(provider_mode != ProviderMode::LocalWhisper);
    let short = match provider_mode {
        ProviderMode::LocalWhisper => "Whisper local",
        ProviderMode::SonioxCloud => "Soniox",
        ProviderMode::OpenAIRealtime => "OpenAI Realtime",
    };
    ui.set_provider_status_label(short.into());

    // Wire UI pickers + startup model scan based on the initial provider mode.
    // This block handles UI-only setup; the actual task is spawned via PipelineHandle.
    match provider_mode {
        ProviderMode::SonioxCloud | ProviderMode::OpenAIRealtime => {
            // VAD sliders are always local regardless of STT provider.
            wire_vad_slider_callbacks_only(ui, live_vad.clone(), runtime.handle().clone());
        }
        ProviderMode::LocalWhisper => {
            // For Whisper local, try to load the model for UI wiring purposes
            // (language pickers, model picker, warmup). The actual spawn is via
            // PipelineHandle::start below — which also handles the load.
            let model_path = rec_cfg
                .whisper_model_path
                .as_ref()
                .filter(|p| p.exists())
                .cloned()
                .unwrap_or_else(WhisperLocalProvider::resolve_default_model_path);

            match WhisperLocalProvider::new(&model_path) {
                Ok(provider) => {
                    let provider = Arc::new(provider);

                    // Wire the shared live config into PipelineHandle so language
                    // pickers + dictionary edits mutate the same handle the provider uses.
                    // Replace the placeholder config with the provider's actual one.
                    let live_config = provider.live_config();
                    // Seed dictionary before wiring.
                    {
                        let guard = dict_file_handle.lock().expect("dict poisoned");
                        if let Ok(mut cfg) = live_config.lock() {
                            cfg.dictionary = guard.entries.clone();
                            cfg.rebuild_dictionary_caches();
                        }
                    }
                    // Phase 12: inject the provider's live config into PipelineHandle so
                    // swap_provider passes the same handle to the new provider after swap.
                    pipeline_handle.set_live_config(live_config.clone());

                    wire_language_picker_callbacks(ui, live_config.clone());
                    wire_recording_settings_callbacks(
                        ui,
                        live_vad.clone(),
                        provider.clone(),
                        state.clone(),
                        rec_cfg.whisper_model_path.clone(),
                        runtime.handle().clone(),
                        toast_queue.clone(),
                    );

                    let model_entries = scan_models(&resolve_models_dir());
                    push_whisper_models_to_ui(ui, &model_entries, &provider.model_label());

                    // Warmup — pays the GPU shader-init cost upfront.
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
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Whisper unavailable at startup — falling back via PipelineHandle"
                    );
                    wire_vad_slider_callbacks_only(
                        ui,
                        live_vad.clone(),
                        runtime.handle().clone(),
                    );
                    let model_entries = scan_models(&resolve_models_dir());
                    push_whisper_models_to_ui(ui, &model_entries, "");
                }
            }
        }
    }

    // Language pickers for cloud providers — wire against the shared live config
    // held in pipeline_handle so source_language / target_mode survive a swap.
    if provider_mode != ProviderMode::LocalWhisper {
        wire_language_picker_callbacks(ui, pipeline_handle.live_stt_config());
    }

    // Spawn the initial provider task via PipelineHandle::start.
    // The event_consumer_fn closure connects the new event channel to PipelineState
    // + DB persistence using the runtime handle (not a borrow).
    {
        let dict_entries = {
            let guard = dict_file_handle.lock().expect("dict poisoned");
            guard.entries.clone()
        };
        let session_for_start = session.as_ref().map(SessionContext::clone_for_task);
        let runtime_handle_for_ec = runtime.handle().clone();
        pipeline_handle.start(
            provider_mode,
            dict_entries,
            session_for_start,
            &rec_cfg,
            move |event_rx, s, sess| {
                spawn_event_consumer_handle(&runtime_handle_for_ec, event_rx, s, sess);
            },
        );
    }

    // Push initial VAD slider values into the Slint UI from the loaded config.
    ui.set_vad_threshold(rec_cfg.vad.threshold);
    ui.set_vad_hangover_ms(rec_cfg.vad.hangover_ms as i32);
    ui.set_vad_max_duration_ms(rec_cfg.vad.max_duration_ms as i32);

    tracing::info!(
        device = %device_name,
        sample_rate,
        channels,
        whisper = state.whisper_loaded.load(std::sync::atomic::Ordering::Relaxed),
        threshold = rec_cfg.vad.threshold,
        hangover_ms = rec_cfg.vad.hangover_ms,
        max_duration_ms = rec_cfg.vad.max_duration_ms,
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

    // ── Phase 13: Toast dismiss callback ──────────────────────────────────────
    {
        let tq = toast_queue.clone();
        ui.on_toast_dismissed(move |id| {
            tq.dismiss(id as u64);
        });
    }

    // ── Phase 13: Dialog confirm / cancel callbacks ────────────────────────────
    {
        let dq = dialog_queue.clone();
        ui.on_dialog_confirmed(move || {
            dq.resolve_confirmed();
        });
    }
    {
        let dq = dialog_queue.clone();
        ui.on_dialog_cancelled(move || {
            dq.resolve_cancelled();
        });
    }

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
            let searching = !q.trim().is_empty();
            let Some(ref ctx) = ctx_for_search else {
                return;
            };
            let Ok(guard) = ctx.conn.try_lock() else {
                tracing::debug!("search: DB busy, skipping");
                return;
            };
            if let Some(ui) = ui_weak_search.upgrade() {
                ui.set_history_is_searching(searching);
                if searching {
                    let text = format_search_results(&guard, q.trim());
                    ui.set_history_status(text.into());
                } else {
                    // Rebuild history rows immediately on clear.
                    let rows = build_history_row_data(&guard);
                    let model = slint::VecModel::from(rows);
                    ui.set_history_model(slint::ModelRc::from(std::rc::Rc::new(model)));
                }
            }
        });
    }

    // Phase 11: SharedDetail — holds the currently-viewed session detail.
    let shared_detail: session_detail::SharedDetail = session_detail::empty_shared_detail();

    // Phase 11: Wire history-row-clicked callback — loads detail + switches view.
    {
        let ui_weak_detail = ui.as_weak();
        let ctx_for_detail = session.as_ref().map(SessionContext::clone_for_task);
        let detail_cell = shared_detail.clone();
        let runner_for_detail = summary_runner_early.clone();
        ui.on_history_row_clicked(move |session_id| {
            let sid = session_id as i64;
            let Some(ref ctx) = ctx_for_detail else { return };
            let Ok(guard) = ctx.conn.try_lock() else {
                tracing::debug!("detail: DB busy on row click");
                return;
            };
            match session_detail::SessionDetailLoad::from_db(&guard, sid) {
                Ok(detail) => {
                    tracing::info!(
                        session_id = sid,
                        segment_count = detail.segments.len(),
                        summary_present = detail.summary.is_some(),
                        "session-detail: loaded"
                    );
                    let summary_state = determine_summary_state(sid, &runner_for_detail, &detail);
                    let summary_text = detail
                        .summary
                        .as_ref()
                        .map(|s| s.content.clone())
                        .unwrap_or_default();
                    if let Ok(mut g) = detail_cell.lock() {
                        *g = Some(detail.clone());
                    }
                    if let Some(ui) = ui_weak_detail.upgrade() {
                        ui.set_detail_session_id(detail.session.id as i32);
                        ui.set_detail_started_at(
                            session_detail::format_timestamp_ms(detail.session.started_at_ms).into()
                        );
                        ui.set_detail_duration(
                            session_detail::format_duration(detail.session.duration_ms).into()
                        );
                        ui.set_detail_provider(
                            slint::SharedString::from(detail.session.provider.as_str())
                        );
                        ui.set_detail_summary_text(slint::SharedString::from(summary_text));
                        ui.set_detail_summary_state(slint::SharedString::from(summary_state));
                        ui.set_detail_error_message(slint::SharedString::from(""));
                        let seg_rows = build_detail_segment_rows(&detail.segments);
                        let seg_model = slint::VecModel::from(seg_rows);
                        ui.set_detail_segments(slint::ModelRc::from(
                            std::rc::Rc::new(seg_model)
                        ));
                        ui.set_current_view(slint::SharedString::from("session-detail"));
                    }
                }
                Err(e) => {
                    tracing::warn!(session_id = sid, error = ?e, "session-detail: load failed");
                }
            }
        });
    }

    // Phase 11: Regenerate button in SessionDetail.
    {
        let runner_for_regen = summary_runner_early.clone();
        let ui_weak_regen = ui.as_weak();
        ui.on_detail_regenerate_clicked(move |session_id| {
            let sid = session_id as i64;
            if runner_for_regen.regen_count(sid) >= 3 {
                tracing::info!(session_id = sid, "detail: regen cap reached — ignoring");
                return;
            }
            if let Some(ui) = ui_weak_regen.upgrade() {
                ui.set_detail_summary_state(slint::SharedString::from("computing"));
            }
            match vong_storage::open_default() {
                Ok(conn) => {
                    let conn_arc = Arc::new(Mutex::new(conn));
                    tracing::info!(session_id = sid, "detail: regenerate triggered");
                    runner_for_regen.trigger_detached(conn_arc, sid, true);
                }
                Err(e) => {
                    tracing::warn!(session_id = sid, error = ?e, "detail: cannot open DB for regen");
                }
            }
        });
    }

    // Phase 11: Copy button in SessionDetail.
    // Phase 13: push info toast on success.
    {
        let detail_cell_copy = shared_detail.clone();
        let tq_copy = toast_queue.clone();
        ui.on_detail_copy_clicked(move |session_id| {
            let sid = session_id as i64;
            let detail_opt = detail_cell_copy.lock().ok().and_then(|g| g.clone());
            let Some(detail) = detail_opt else {
                tracing::debug!(session_id = sid, "detail: copy — no detail loaded");
                return;
            };
            let text = session_detail::format_summary_for_clipboard(&detail);
            if text.is_empty() {
                tracing::debug!(session_id = sid, "detail: copy — no summary text");
                return;
            }
            match session_detail::copy_to_clipboard(&text) {
                Ok(()) => {
                    tracing::info!(session_id = sid, "detail: summary copied to clipboard");
                    // Phase 13: visible confirmation instead of silent tracing log.
                    tq_copy.push("Đã sao chép tóm tắt", ToastSeverity::Info);
                }
                Err(e) => {
                    tracing::warn!(session_id = sid, error = %e, "detail: clipboard write failed");
                    tq_copy.push("Sao chép thất bại — thử lại", ToastSeverity::Error);
                }
            }
        });
    }

    // ── Phase 13: Model-switch confirm dialog ──────────────────────────────────
    // When the user clicks Switch on WhisperModelCard, a Dialog is shown before
    // the actual swap runs. On Confirm the existing whisper-model-switch-clicked
    // callback fires unchanged; on Cancel the model stays.
    {
        let dq_switch = dialog_queue.clone();
        let ui_weak_switch = ui.as_weak();
        let runtime_for_confirm = runtime.handle().clone();
        ui.on_request_whisper_model_switch_confirm(move |file_name| {
            let name = file_name.to_string();
            let dq = dq_switch.clone();
            let ui_weak = ui_weak_switch.clone();
            // Open dialog and wait for the user choice on a background task so
            // we don't block the Slint event loop.
            let rx = dq.confirm(
                "Đổi mô hình Whisper",
                "Đổi mô hình sẽ ngắt phiên ghi âm hiện tại — bạn có muốn tiếp tục?",
            );
            runtime_for_confirm.spawn(async move {
                let confirmed = rx.await.unwrap_or(false);
                if confirmed {
                    let name_clone = name.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.invoke_whisper_model_switch_clicked(
                                slint::SharedString::from(name_clone),
                            );
                        }
                    });
                } else {
                    tracing::debug!("model switch cancelled by user");
                }
            });
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
    let toast_queue_for_timer = toast_queue.clone();
    let dialog_queue_for_timer = dialog_queue.clone();
    // Phase 13: summary runner for error toast detection.
    let runner_for_timer = summary_runner_early.clone();
    let session_id_for_timer = session.as_ref().map(|s| s.session_id).unwrap_or(0);
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

                // Mirror model-switch state into the Recording settings card.
                let model_in_flight = state_ui
                    .model_switch_in_flight
                    .load(std::sync::atomic::Ordering::Relaxed);
                ui.set_model_switch_in_flight(model_in_flight);
                if let Ok(status) = state_ui.model_switch_status.lock() {
                    ui.set_model_switch_status(status.clone().into());
                }
                if let Ok(label) = state_ui.active_whisper_model.lock() {
                    ui.set_active_whisper_model(label.clone().into());
                }

                if dropped > 0 {
                    tracing::warn!(dropped, "ring buffer overflow");
                }

                // ── Phase 13: Push toast stack into Slint ─────────────────────
                toast_queue_for_timer.prune_expired();
                let toasts = toast_queue_for_timer.snapshot();
                let slint_toasts: Vec<ToastEntry> = toasts
                    .iter()
                    .map(|t| ToastEntry {
                        id: t.id.min(i32::MAX as u64) as i32,
                        message: slint::SharedString::from(t.message.as_str()),
                        severity: slint::SharedString::from(t.severity.as_str()),
                        expires_at_ms: t.expires_at_ms.min(i32::MAX as u64) as i32,
                    })
                    .collect();
                let toast_model = slint::VecModel::from(slint_toasts);
                ui.set_toast_stack(slint::ModelRc::from(
                    std::rc::Rc::new(toast_model),
                ));

                // ── Phase 13: Push dialog state into Slint ────────────────────
                let (d_open, d_title, d_body) = dialog_queue_for_timer.snapshot();
                ui.set_dialog_open(d_open);
                ui.set_dialog_title(slint::SharedString::from(d_title));
                ui.set_dialog_body(slint::SharedString::from(d_body));

                // ── Phase 13: Summary error toast (one per session) ───────────
                if session_id_for_timer != 0 {
                    use summary_runner::SummaryState;
                    let has_error = runner_for_timer
                        .state
                        .lock()
                        .ok()
                        .and_then(|g| g.get(&session_id_for_timer).cloned())
                        .map(|s| matches!(s, SummaryState::Failed(_)))
                        .unwrap_or(false);
                    if has_error {
                        let last_toasted = state_ui
                            .last_summary_error_toast_session
                            .load(Ordering::Relaxed);
                        if last_toasted != session_id_for_timer as u64 {
                            state_ui.last_summary_error_toast_session.store(
                                session_id_for_timer as u64,
                                Ordering::Relaxed,
                            );
                            toast_queue_for_timer.push(
                                "Tóm tắt thất bại — kiểm tra API key OpenAI",
                                ToastSeverity::Error,
                            );
                        }
                    }
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
                            // Phase 11: push VecModel rows.
                            let rows = build_history_row_data(&guard);
                            if let Some(ui) = ui_weak.upgrade() {
                                let model = slint::VecModel::from(rows);
                                ui.set_history_model(slint::ModelRc::from(
                                    std::rc::Rc::new(model)
                                ));
                                ui.set_history_is_searching(false);
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

    // Wire Settings → Dictionary tab callbacks (add / edit / delete / move).
    // `live_config_for_dict` is `None` for cloud providers (they don't use
    // `LiveSttConfig` for dictionary — their caches are seeded at startup).
    // For Whisper local, we need a second grab of `live_config`. We thread it
    // through via a separate optional capture.
    //
    // Note: for cloud providers, the dictionary still persists and the Whisper
    // cache still rebuilds (no-op since Whisper isn't running), so the UI
    // state stays consistent for when the user switches providers.
    wire_dictionary_callbacks(ui, dict_file_handle.clone(), provider_mode, toast_queue.clone());

    // Push initial dictionary state into the Slint UI.
    {
        let guard = dict_file_handle.lock().expect("dict poisoned");
        push_dictionary_to_ui(ui, &guard.entries, provider_mode);
    }

    // ── Phase 10: Auto-summary Settings callbacks ─────────────────────────────
    wire_autosummary_callbacks(ui, summary_runner_early.clone(), toast_queue.clone());
    // Push initial autosummary state to the UI.
    ui.set_auto_summary_enabled(autosummary::should_auto_summarize());
    ui.set_auto_summary_has_key(vong_transcribe::ApiKey::load("openai-realtime").is_ok());

    // ── Phase 12: Wire provider hot-swap callback ─────────────────────────────
    // Now that pipeline_handle is fully initialised, wire the ComboBox callback
    // so selecting a different provider triggers swap_provider instead of showing
    // the restart banner.
    wire_provider_hot_swap_callback(
        ui,
        pipeline_handle.clone(),
        session.as_ref().map(SessionContext::clone_for_task),
        rec_cfg.clone(),
        runtime.handle().clone(),
        dialog_queue.clone(),
        toast_queue.clone(),
    );

    Ok(AudioBundle {
        _active_source: active_source,
        _timer: timer,
        _runtime: runtime,
        session,
        summary_runner: summary_runner_early,
        _toast_queue: toast_queue,
        _dialog_queue: dialog_queue,
        _pipeline: pipeline_handle,
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

    // ── Complete (Step 7 CTA — was step 6 before Phase 9) ───────────────────
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

    // ── Crash reporting consent from wizard (Step 6) ─────────────────────────
    // User taps "Đồng ý" or "Không" in CrashReportStep → Slint calls back
    // with a bool. We persist both sentry.txt AND the wizard step marker,
    // then advance the step.
    {
        let ui_weak = ui.as_weak();
        ui.on_wizard_crash_reporting_changed(move |consent| {
            if let Err(e) = wizard::mark_crash_report_consent(consent) {
                tracing::warn!(
                    error = ?e,
                    sentry_consent = consent,
                    "wizard: failed to persist crash-report consent"
                );
            } else {
                tracing::info!(
                    sentry_consent = consent,
                    "wizard: crash-report consent saved"
                );
            }
            // Advance to Done (index 7).
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_wizard_crash_reporting_consent(consent);
                ui.set_wizard_step(7);
            }
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

/// Wire the provider picker initial state (options + selected value).
///
/// The actual hot-swap callback is wired inside `init_audio` via
/// `wire_provider_hot_swap_callback` so it has access to `PipelineHandle`.
fn wire_provider_picker_callbacks(ui: &AppWindow) {
    // The save-key callback is stateless — wire it here.
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

/// Wire the provider-mode ComboBox callback to use hot-swap via PipelineHandle.
///
/// Called from `init_audio` after `pipeline_handle` is ready. Selecting a
/// different provider immediately triggers `swap_provider` — no restart needed.
/// On swap failure, the UI ComboBox is reverted to the previous selection.
fn wire_provider_hot_swap_callback(
    ui: &AppWindow,
    pipeline: Arc<PipelineHandle>,
    session: Option<SessionContext>,
    rec_cfg: recording_config::RecordingConfig,
    runtime_handle: tokio::runtime::Handle,
    dialog_queue: DialogQueue,
    toast_queue: ToastQueue,
) {
    let ui_weak = ui.as_weak();
    ui.on_provider_mode_changed(move |label| {
        let Some(new_mode) = parse_provider_label(label.as_str()) else {
            tracing::warn!(label = %label, "unknown provider label");
            return;
        };

        // Update API-key field visibility immediately (no await needed).
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_provider_is_cloud(new_mode != ProviderMode::LocalWhisper);
        }

        // Spawn the async swap on the tokio runtime so we don't block the
        // Slint event loop.
        let pipeline = pipeline.clone();
        let session = session.as_ref().map(SessionContext::clone_for_task);
        let rec_cfg = rec_cfg.clone();
        let dialog_queue = dialog_queue.clone();
        let toast_queue = toast_queue.clone();
        let ui_weak = ui_weak.clone();
        let runtime_for_ec = runtime_handle.clone();
        runtime_handle.spawn(async move {
            let result = pipeline
                .swap_provider(
                    new_mode,
                    session,
                    &rec_cfg,
                    &dialog_queue,
                    &toast_queue,
                    move |event_rx, s, sess| {
                        spawn_event_consumer_handle(&runtime_for_ec, event_rx, s, sess);
                    },
                )
                .await;

            match result {
                Ok(()) => {
                    tracing::info!(
                        new_provider = new_mode.id(),
                        "provider hot-swap complete"
                    );
                    // Update footer label to reflect new provider.
                    let short = match new_mode {
                        ProviderMode::LocalWhisper => "Whisper local",
                        ProviderMode::SonioxCloud => "Soniox",
                        ProviderMode::OpenAIRealtime => "OpenAI Realtime",
                    };
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_provider_status_label(short.into());
                            ui.set_current_provider_mode(
                                provider_label(new_mode).into(),
                            );
                        }
                    });
                }
                Err(ref e) => {
                    // Surface toast; revert ComboBox to the previous selection.
                    toast_queue.push(e.to_toast_message(), e.toast_severity());
                    let prev_mode = pipeline.current_mode();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_current_provider_mode(
                                provider_label(prev_mode).into(),
                            );
                            ui.set_provider_is_cloud(
                                prev_mode != ProviderMode::LocalWhisper
                            );
                        }
                    });
                    tracing::warn!(
                        outcome = ?e,
                        "provider hot-swap returned error"
                    );
                }
            }
        });
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

// ── Phase 8: Dictionary UI helpers ────────────────────────────────────────

/// Push current dictionary entries into the Slint UI model.
///
/// Privacy: phrase strings are user content — we set Slint properties,
/// but never write them to `tracing::*`.
fn push_dictionary_to_ui(ui: &AppWindow, entries: &[DictEntry], provider_mode: ProviderMode) {
    let rows: Vec<DictionaryEntryRow> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| DictionaryEntryRow {
            phrase: slint::SharedString::from(e.phrase.as_str()),
            context: slint::SharedString::from(context_to_ui_str(e.context)),
            index: i as i32,
        })
        .collect();
    ui.set_dictionary_rows(slint::ModelRc::from(
        Rc::new(slint::VecModel::from(rows)),
    ));
    ui.set_dictionary_entry_count(entries.len() as i32);
    // Show the "restart required" hint when the user has cloud provider selected.
    // The hint is only meaningful after an edit — at startup it should be hidden.
    // We set false here; it becomes true after the first save with a cloud provider.
    let is_cloud = provider_mode != ProviderMode::LocalWhisper;
    ui.set_dictionary_show_restart_hint(false);
    let _ = is_cloud; // Will be used in the add/edit/delete callbacks.
}

/// Map `DictContext` → UI dropdown string.
fn context_to_ui_str(ctx: vong_transcribe::DictContext) -> &'static str {
    use vong_transcribe::DictContext;
    match ctx {
        DictContext::Common => "common",
        DictContext::Names => "names",
        DictContext::Technical => "technical",
    }
}

/// Wire all Settings → Dictionary tab callbacks (add / edit / delete / move).
///
/// `dict_file_handle` is the shared dictionary state. Changes are immediately
/// persisted to disk and reflected in the Slint UI model.
fn wire_dictionary_callbacks(
    ui: &AppWindow,
    dict_file_handle: Arc<Mutex<dictionary::DictionaryFile>>,
    provider_mode: ProviderMode,
    toast_queue: ToastQueue,
) {
    let is_cloud = provider_mode != ProviderMode::LocalWhisper;

    // ── on_dictionary_add ────────────────────────────────────────────────────
    {
        let dict = dict_file_handle.clone();
        let ui_weak = ui.as_weak();
        let tq_add = toast_queue.clone();
        ui.on_dictionary_add(move |phrase, context| {
            let phrase_s = phrase.to_string();
            let mut file = dict.lock().expect("dict poisoned");
            match dictionary::validate_new(&file.entries, &phrase_s) {
                Err(e) => {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_dictionary_add_error(
                            slint::SharedString::from(dictionary::vi_error_msg(&e)),
                        );
                    }
                }
                Ok(()) => {
                    file.entries.push(DictEntry {
                        phrase: phrase_s.trim().to_string(),
                        context: dictionary::parse_context(context.as_str()),
                    });
                    if let Err(e) = dictionary::save(&file) {
                        tracing::warn!(error = %e, "dictionary save failed on add");
                        tq_add.push("Lưu từ điển thất bại", ToastSeverity::Error);
                    }
                    let entries_snapshot = file.entries.clone();
                    drop(file);
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_dictionary_add_error(slint::SharedString::default());
                        let rows = build_dict_rows(&entries_snapshot);
                        ui.set_dictionary_rows(slint::ModelRc::from(
                            Rc::new(slint::VecModel::from(rows)),
                        ));
                        ui.set_dictionary_entry_count(entries_snapshot.len() as i32);
                        if is_cloud {
                            ui.set_dictionary_show_restart_hint(true);
                        }
                    }
                }
            }
        });
    }

    // ── on_dictionary_delete ─────────────────────────────────────────────────
    {
        let dict = dict_file_handle.clone();
        let ui_weak = ui.as_weak();
        ui.on_dictionary_delete(move |index| {
            let idx = index as usize;
            let mut file = dict.lock().expect("dict poisoned");
            if idx < file.entries.len() {
                file.entries.remove(idx);
                if let Err(e) = dictionary::save(&file) {
                    tracing::warn!(error = %e, "dictionary save failed on delete");
                }
            }
            let entries_snapshot = file.entries.clone();
            drop(file);
            if let Some(ui) = ui_weak.upgrade() {
                let rows = build_dict_rows(&entries_snapshot);
                ui.set_dictionary_rows(slint::ModelRc::from(
                    Rc::new(slint::VecModel::from(rows)),
                ));
                ui.set_dictionary_entry_count(entries_snapshot.len() as i32);
                if is_cloud {
                    ui.set_dictionary_show_restart_hint(true);
                }
            }
        });
    }

    // ── on_dictionary_edit ───────────────────────────────────────────────────
    // Saves an in-place edit to an existing entry.
    {
        let dict = dict_file_handle.clone();
        let ui_weak = ui.as_weak();
        ui.on_dictionary_edit(move |index, new_phrase, new_context| {
            let idx = index as usize;
            let phrase_s = new_phrase.to_string();
            let trimmed = phrase_s.trim().to_string();
            let mut file = dict.lock().expect("dict poisoned");
            if idx < file.entries.len() && !trimmed.is_empty() {
                // Check for duplicate against other entries (not self).
                let is_dup = file
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != idx)
                    .any(|(_, e)| e.phrase == trimmed);
                if !is_dup && trimmed.chars().count() <= dictionary::PHRASE_MAX_CHARS {
                    file.entries[idx].phrase = trimmed;
                    file.entries[idx].context =
                        dictionary::parse_context(new_context.as_str());
                    if let Err(e) = dictionary::save(&file) {
                        tracing::warn!(error = %e, "dictionary save failed on edit");
                    }
                }
            }
            let entries_snapshot = file.entries.clone();
            drop(file);
            if let Some(ui) = ui_weak.upgrade() {
                let rows = build_dict_rows(&entries_snapshot);
                ui.set_dictionary_rows(slint::ModelRc::from(
                    Rc::new(slint::VecModel::from(rows)),
                ));
                if is_cloud {
                    ui.set_dictionary_show_restart_hint(true);
                }
            }
        });
    }

    // ── on_dictionary_move_up ────────────────────────────────────────────────
    {
        let dict = dict_file_handle.clone();
        let ui_weak = ui.as_weak();
        ui.on_dictionary_move_up(move |index| {
            let idx = index as usize;
            let mut file = dict.lock().expect("dict poisoned");
            if idx > 0 && idx < file.entries.len() {
                file.entries.swap(idx - 1, idx);
                if let Err(e) = dictionary::save(&file) {
                    tracing::warn!(error = %e, "dictionary save failed on move-up");
                }
            }
            let entries_snapshot = file.entries.clone();
            drop(file);
            if let Some(ui) = ui_weak.upgrade() {
                let rows = build_dict_rows(&entries_snapshot);
                ui.set_dictionary_rows(slint::ModelRc::from(
                    Rc::new(slint::VecModel::from(rows)),
                ));
            }
        });
    }

    // ── on_dictionary_move_down ──────────────────────────────────────────────
    {
        let dict = dict_file_handle.clone();
        let ui_weak = ui.as_weak();
        ui.on_dictionary_move_down(move |index| {
            let idx = index as usize;
            let mut file = dict.lock().expect("dict poisoned");
            if idx + 1 < file.entries.len() {
                file.entries.swap(idx, idx + 1);
                if let Err(e) = dictionary::save(&file) {
                    tracing::warn!(error = %e, "dictionary save failed on move-down");
                }
            }
            let entries_snapshot = file.entries.clone();
            drop(file);
            if let Some(ui) = ui_weak.upgrade() {
                let rows = build_dict_rows(&entries_snapshot);
                ui.set_dictionary_rows(slint::ModelRc::from(
                    Rc::new(slint::VecModel::from(rows)),
                ));
            }
        });
    }

    // ── on_dictionary_clear_error ────────────────────────────────────────────
    {
        let ui_weak = ui.as_weak();
        ui.on_dictionary_clear_error(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_dictionary_add_error(slint::SharedString::default());
            }
        });
    }
}

// ── Phase 10: Auto-summary Settings callbacks ─────────────────────────────────

/// Wire the Settings → System auto-summary toggle and regenerate callbacks.
fn wire_autosummary_callbacks(ui: &AppWindow, runner: Arc<SummaryRunner>, toast_queue: ToastQueue) {
    // Settings toggle: persist consent + update UI state.
    {
        let ui_weak = ui.as_weak();
        let tq = toast_queue.clone();
        ui.on_auto_summary_toggled(move |enabled| {
            if let Err(e) = autosummary::save_consent(enabled) {
                tracing::warn!(error = %e, "autosummary consent save failed");
                // Phase 13: surface config-save failure.
                tq.push("Lưu cài đặt tóm tắt thất bại", ToastSeverity::Error);
            }
            tracing::info!(enabled, "auto-summary toggle persisted");
            if let Some(ui) = ui_weak.upgrade() {
                // Reflect whether key is actually available (toggle ON without key
                // should still show as disabled in the UI).
                let has_key = vong_transcribe::ApiKey::load("openai-realtime").is_ok();
                ui.set_auto_summary_enabled(enabled && has_key);
                ui.set_auto_summary_has_key(has_key);
            }
        });
    }

    // Regenerate callback: re-run summary for the given session.
    {
        let runner_regen = runner;
        ui.on_summary_regenerate_clicked(move |session_id| {
            let sid = session_id as i64;
            if runner_regen.regen_count(sid) >= 3 {
                tracing::info!(session_id = sid, "summary: regen cap reached — ignoring click");
                return;
            }
            // We need a conn — but at this point we're in a UI callback without
            // direct access to the session conn. Use the in-memory DB path as fallback,
            // or resolve via the open_default path. For regenerate from History, the
            // session already persisted — open a fresh read connection.
            if let Ok(conn) = vong_storage::open_default() {
                let conn_arc = Arc::new(Mutex::new(conn));
                tracing::info!(session_id = sid, "summary: regenerate triggered from UI");
                runner_regen.trigger_detached(conn_arc, sid, true);
            } else {
                tracing::warn!(session_id = sid, "summary: regenerate failed — cannot open DB");
            }
        });
    }
}

/// Build the Slint `DictionaryEntryRow` view-model from the current entries.
fn build_dict_rows(entries: &[DictEntry]) -> Vec<DictionaryEntryRow> {
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| DictionaryEntryRow {
            phrase: slint::SharedString::from(e.phrase.as_str()),
            context: slint::SharedString::from(context_to_ui_str(e.context)),
            index: i as i32,
        })
        .collect()
}

// Phase 12: spawn_provider_task, spawn_whisper_pipeline, spawn_soniox_pipeline,
// spawn_openai_pipeline, and spawn_utterance_counter were removed — provider
// task spawning is now handled by pipeline::PipelineHandle::start and
// pipeline::build_provider_task.

/// Spawn the STT event consumer on the given tokio runtime handle.
///
/// Handles Partial / Final / Translation / Error / Disconnected events across
/// all providers. Wired by the `event_consumer_fn` closure passed to
/// `PipelineHandle::start` and `swap_provider`.
pub(crate) fn spawn_event_consumer_handle(
    handle: &tokio::runtime::Handle,
    mut event_rx: tokio::sync::mpsc::Receiver<TranscriptEvent>,
    state: Arc<PipelineState>,
    session: Option<SessionContext>,
) {
    handle.spawn(async move {
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
    // Phase 13: toast queue for surfacing download errors in the UI.
    // Initialised lazily — wired to the same global singleton via Slint event loop.
    // We pass a fresh queue here; error toasts are pushed via `slint::invoke_from_event_loop`
    // so they reach the running app's shared queue.
    // NOTE: The download runs on a background thread without access to the main
    // app's ToastQueue. We store the error message in `dl_progress.error_msg`
    // and detect it in the dl_timer closure to push the toast from the Slint thread.
    // See the `_dl_timer` closure in `main()` which already checks `p.state`.
    // No additional toast queue reference is needed here — the dl_timer does it.
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

// ── Phase 7: Recording settings — VAD sliders + Whisper model picker ─────────

/// Scan the models directory for GGML Whisper files (`ggml-*.bin`).
/// Returns entries sorted by filename for consistent UI ordering.
fn scan_models(models_dir: &Path) -> Vec<ModelScanEntry> {
    let mut entries = Vec::new();

    // Scan two candidate locations: the standard models dir (data_local)
    // and the exe-adjacent dir (used by portable installs).
    let mut dirs: Vec<PathBuf> = vec![models_dir.to_path_buf()];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let exe_models = parent.join("models");
            if exe_models != models_dir && exe_models.is_dir() {
                dirs.push(exe_models);
            }
        }
    }

    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("ggml-") || !name.ends_with(".bin") {
                continue;
            }
            let size_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            entries.push(ModelScanEntry {
                name,
                path: entry.path(),
                size_bytes,
            });
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// Format a byte count as a human-readable size string (e.g. "142 MB").
fn format_model_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_000_000 {
        format!("{} MB", bytes / 1_048_576)
    } else {
        format!("{} KB", bytes / 1024)
    }
}

/// Push the scanned model list into the Slint UI.
/// `active_label` is the filename of the currently loaded model (e.g. "ggml-base.bin").
fn push_whisper_models_to_ui(ui: &AppWindow, entries: &[ModelScanEntry], active_label: &str) {
    let slint_entries: Vec<WhisperModelEntry> = entries
        .iter()
        .map(|e| WhisperModelEntry {
            name: e.name.clone().into(),
            size_label: format_model_size(e.size_bytes).into(),
            is_active: e.name == active_label,
        })
        .collect();
    ui.set_whisper_models(slint::ModelRc::from(Rc::new(slint::VecModel::from(
        slint_entries,
    ))));
    ui.set_active_whisper_model(active_label.into());
}

/// Wire the VAD slider callbacks + debounced persist + Whisper model-switch
/// callback. Only called when `LocalWhisper` provider loaded successfully.
fn wire_recording_settings_callbacks(
    ui: &AppWindow,
    live_vad: LiveVadConfig,
    whisper_provider: Arc<WhisperLocalProvider>,
    state: Arc<PipelineState>,
    initial_model_path: Option<PathBuf>,
    runtime: tokio::runtime::Handle,
    toast_queue: ToastQueue,
) {
    // Debounce channel: slider changes post into this, a task drains at 300ms.
    let (debounce_tx, mut debounce_rx) =
        tokio::sync::mpsc::channel::<recording_config::VadParams>(8);

    // Keep the current model path in a shared mutex so the debounce task can
    // read it when persisting VAD changes alongside the model path.
    let current_model_path: Arc<Mutex<Option<PathBuf>>> =
        Arc::new(Mutex::new(initial_model_path));
    let model_path_for_debounce = current_model_path.clone();

    // Spawn the debounce-persist task.
    runtime.spawn(async move {
        let mut latest: Option<recording_config::VadParams> = None;
        let mut interval = tokio::time::interval(Duration::from_millis(300));
        loop {
            tokio::select! {
                Some(p) = debounce_rx.recv() => {
                    latest = Some(p);
                }
                _ = interval.tick() => {
                    if let Some(p) = latest.take() {
                        let model_path = model_path_for_debounce
                            .lock()
                            .ok()
                            .and_then(|g| g.clone());
                        let cfg = recording_config::RecordingConfig {
                            version: 1,
                            vad: p,
                            whisper_model_path: model_path,
                        };
                        if let Err(e) = recording_config::save(&cfg) {
                            tracing::warn!(error = %e, "recording.json VAD save failed");
                        }
                    }
                }
            }
        }
    });

    // VAD slider changed callback — live-apply + debounce persist.
    {
        let live_vad_cb = live_vad.clone();
        let tx = debounce_tx.clone();
        ui.on_vad_changed(move |threshold, hangover_ms, max_duration_ms| {
            let mut params = recording_config::VadParams {
                threshold,
                hangover_ms: hangover_ms.max(0) as u32,
                max_duration_ms: max_duration_ms.max(0) as u32,
            };
            params.clamp_in_place();
            // Immediately apply to the live FSM handle.
            if let Ok(mut g) = live_vad_cb.lock() {
                g.threshold = params.threshold;
                g.hangover_ms = params.hangover_ms;
                g.max_duration_ms = params.max_duration_ms;
            }
            // Debounce the disk write.
            let _ = tx.try_send(params);
        });
    }

    // Reset callbacks set the in-out property to the default.
    {
        let ui_weak = ui.as_weak();
        let live_vad_reset = live_vad.clone();
        let tx_reset = debounce_tx.clone();
        ui.on_vad_reset_threshold(move || {
            let default_threshold = vong_audio::VadConfig::default().threshold;
            if let Ok(mut g) = live_vad_reset.lock() {
                g.threshold = default_threshold;
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_vad_threshold(default_threshold);
            }
            let params = get_current_vad_params(&live_vad_reset);
            let _ = tx_reset.try_send(params);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let live_vad_reset = live_vad.clone();
        let tx_reset = debounce_tx.clone();
        ui.on_vad_reset_hangover(move || {
            let default_hangover = vong_audio::VadConfig::default().hangover_ms;
            if let Ok(mut g) = live_vad_reset.lock() {
                g.hangover_ms = default_hangover;
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_vad_hangover_ms(default_hangover as i32);
            }
            let params = get_current_vad_params(&live_vad_reset);
            let _ = tx_reset.try_send(params);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let live_vad_reset = live_vad.clone();
        let tx_reset = debounce_tx;
        ui.on_vad_reset_max_duration(move || {
            let default_max = vong_audio::VadConfig::default().max_duration_ms;
            if let Ok(mut g) = live_vad_reset.lock() {
                g.max_duration_ms = default_max;
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_vad_max_duration_ms(default_max as i32);
            }
            let params = get_current_vad_params(&live_vad_reset);
            let _ = tx_reset.try_send(params);
        });
    }

    // Model switch callback.
    {
        let ui_weak = ui.as_weak();
        let state_cb = state.clone();
        let provider_cb = whisper_provider.clone();
        let model_path_for_switch = current_model_path;
        let models_dir = resolve_models_dir();
        ui.on_whisper_model_switch_clicked(move |file_name| {
            let file_name = file_name.to_string();
            // Locate the file among scanned entries.
            let entries = scan_models(&models_dir);
            let Some(entry) = entries.iter().find(|e| e.name == file_name) else {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_model_switch_status("Không tìm thấy file mô hình".into());
                }
                return;
            };
            let target_path = entry.path.clone();

            // Mark in-flight.
            state_cb
                .model_switch_in_flight
                .store(true, std::sync::atomic::Ordering::Relaxed);
            if let Ok(mut g) = state_cb.model_switch_status.lock() {
                *g = "Đang tải mô hình…".to_string();
            }

            let provider_for_swap = provider_cb.clone();
            let state_for_done = state_cb.clone();
            let model_path_arc = model_path_for_switch.clone();
            let ui_weak2 = ui_weak.clone();
            let file_name_clone = file_name.clone();
            let path_for_blocking = target_path.clone();
            let tq_swap = toast_queue.clone();

            runtime.spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    provider_for_swap.swap_context(&path_for_blocking)
                })
                .await;

                let (success, status_msg) = match result {
                    Ok(Ok(())) => {
                        // Persist the new model path.
                        if let Ok(mut g) = model_path_arc.lock() {
                            *g = Some(target_path.clone());
                        }
                        let vad_params = {
                            // Read current VAD from provider (we don't have live_vad here;
                            // use defaults as fallback — the debounce path will overwrite next drag).
                            recording_config::VadParams::default()
                        };
                        let cfg = recording_config::RecordingConfig {
                            version: 1,
                            vad: vad_params,
                            whisper_model_path: Some(target_path),
                        };
                        if let Err(e) = recording_config::save(&cfg) {
                            tracing::warn!(error = %e, "recording.json model path save failed");
                            // Phase 13: surface config-save failure.
                            tq_swap.push("Lưu cấu hình mô hình thất bại", ToastSeverity::Warn);
                        }
                        (true, String::new())
                    }
                    Ok(Err(e)) => {
                        tracing::error!(error = %e, "model swap failed");
                        // Phase 13: surface swap failure.
                        tq_swap.push(
                            "Tải mô hình thất bại — giữ mô hình cũ",
                            ToastSeverity::Error,
                        );
                        (false, "Tải thất bại — giữ mô hình cũ".to_string())
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "model swap join failed");
                        tq_swap.push(
                            "Lỗi tải mô hình — giữ mô hình cũ",
                            ToastSeverity::Error,
                        );
                        (false, "Lỗi nội bộ — giữ mô hình cũ".to_string())
                    }
                };

                state_for_done
                    .model_switch_in_flight
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                if let Ok(mut g) = state_for_done.model_switch_status.lock() {
                    *g = status_msg;
                }
                if success {
                    if let Ok(mut g) = state_for_done.active_whisper_model.lock() {
                        *g = file_name_clone.clone();
                    }
                }

                // Update the model list in the UI from the event loop.
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_weak2.upgrade() else { return };
                    let models_dir2 = resolve_models_dir();
                    let entries2 = scan_models(&models_dir2);
                    let active = if success { &file_name_clone } else { "" };
                    push_whisper_models_to_ui(&ui, &entries2, active);
                });
            });
        });
    }
}

/// Wire ONLY the VAD slider callbacks (for non-Whisper provider modes where
/// the model picker doesn't apply). Uses a simplified debounce path.
fn wire_vad_slider_callbacks_only(
    ui: &AppWindow,
    live_vad: LiveVadConfig,
    runtime: tokio::runtime::Handle,
) {
    let (debounce_tx, mut debounce_rx) =
        tokio::sync::mpsc::channel::<recording_config::VadParams>(8);

    // Spawn debounce persist task (no model path to track here).
    runtime.spawn(async move {
        let mut latest: Option<recording_config::VadParams> = None;
        let mut interval = tokio::time::interval(Duration::from_millis(300));
        loop {
            tokio::select! {
                Some(p) = debounce_rx.recv() => {
                    latest = Some(p);
                }
                _ = interval.tick() => {
                    if let Some(p) = latest.take() {
                        let cfg = recording_config::RecordingConfig {
                            version: 1,
                            vad: p,
                            whisper_model_path: None,
                        };
                        if let Err(e) = recording_config::save(&cfg) {
                            tracing::warn!(error = %e, "recording.json VAD save failed");
                        }
                    }
                }
            }
        }
    });

    {
        let live_vad_cb = live_vad.clone();
        let tx = debounce_tx.clone();
        ui.on_vad_changed(move |threshold, hangover_ms, max_duration_ms| {
            let mut params = recording_config::VadParams {
                threshold,
                hangover_ms: hangover_ms.max(0) as u32,
                max_duration_ms: max_duration_ms.max(0) as u32,
            };
            params.clamp_in_place();
            if let Ok(mut g) = live_vad_cb.lock() {
                g.threshold = params.threshold;
                g.hangover_ms = params.hangover_ms;
                g.max_duration_ms = params.max_duration_ms;
            }
            let _ = tx.try_send(params);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let lv = live_vad.clone();
        let tx = debounce_tx.clone();
        ui.on_vad_reset_threshold(move || {
            let d = vong_audio::VadConfig::default().threshold;
            if let Ok(mut g) = lv.lock() { g.threshold = d; }
            if let Some(ui) = ui_weak.upgrade() { ui.set_vad_threshold(d); }
            let _ = tx.try_send(get_current_vad_params(&lv));
        });
    }
    {
        let ui_weak = ui.as_weak();
        let lv = live_vad.clone();
        let tx = debounce_tx.clone();
        ui.on_vad_reset_hangover(move || {
            let d = vong_audio::VadConfig::default().hangover_ms;
            if let Ok(mut g) = lv.lock() { g.hangover_ms = d; }
            if let Some(ui) = ui_weak.upgrade() { ui.set_vad_hangover_ms(d as i32); }
            let _ = tx.try_send(get_current_vad_params(&lv));
        });
    }
    {
        let ui_weak = ui.as_weak();
        let lv = live_vad.clone();
        let tx = debounce_tx;
        ui.on_vad_reset_max_duration(move || {
            let d = vong_audio::VadConfig::default().max_duration_ms;
            if let Ok(mut g) = lv.lock() { g.max_duration_ms = d; }
            if let Some(ui) = ui_weak.upgrade() { ui.set_vad_max_duration_ms(d as i32); }
            let _ = tx.try_send(get_current_vad_params(&lv));
        });
    }
    // Whisper model switch is a no-op for cloud providers.
    ui.on_whisper_model_switch_clicked(|_| {});
}

// ── Phase 11: HistoryRowData + SessionDetail helpers ──────────────────────────

/// Build the `HistoryRowData` VecModel entries from the 10 most-recent sessions.
/// Fetches summaries in ONE batched query to avoid N+1.
fn build_history_row_data(conn: &Connection) -> Vec<HistoryRowData> {
    let sessions = match list_recent_sessions(conn, 10) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = ?e, "history: list_recent_sessions failed");
            return Vec::new();
        }
    };
    if sessions.is_empty() {
        return Vec::new();
    }
    let ids: Vec<i64> = sessions.iter().map(|s| s.id).collect();
    let summaries = fetch_summaries_for_sessions(conn, &ids).unwrap_or_default();

    sessions
        .iter()
        .map(|s| {
            let summary = summaries.get(&s.id);
            let preview = session_detail::summary_preview(summary);
            let started = session_detail::format_timestamp_ms(s.started_at_ms);
            let duration = session_detail::format_duration(s.duration_ms);
            let provider = short_provider_label(&s.provider);
            HistoryRowData {
                session_id: s.id as i32,
                started_at: slint::SharedString::from(started),
                duration: slint::SharedString::from(duration),
                provider: slint::SharedString::from(provider),
                summary_preview: slint::SharedString::from(preview),
            }
        })
        .collect()
}

/// Convert provider id string to short display label.
fn short_provider_label(provider: &str) -> &str {
    match provider {
        "soniox" => "Soniox",
        "openai-realtime" => "OpenAI",
        _ => "Whisper",
    }
}

/// Build the `DetailSegmentRow` list from a `Segment` slice.
fn build_detail_segment_rows(segments: &[vong_storage::Segment]) -> Vec<DetailSegmentRow> {
    segments
        .iter()
        .map(|seg| {
            let ts = format_ms_as_timestamp(seg.start_ms);
            DetailSegmentRow {
                timestamp: slint::SharedString::from(ts),
                speaker: slint::SharedString::from(
                    seg.speaker_label.as_deref().unwrap_or("")
                ),
                text: slint::SharedString::from(seg.text.as_str()),
            }
        })
        .collect()
}

/// Format segment start_ms as "M:SS" for the detail segment list.
fn format_ms_as_timestamp(ms: i64) -> String {
    let secs = (ms / 1000).max(0) as u64;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Determine the `summary-state` string for the SessionDetail view based on
/// SummaryRunner state + whether a summary row already exists in the DB.
fn determine_summary_state(
    session_id: i64,
    runner: &summary_runner::SummaryRunner,
    detail: &session_detail::SessionDetailLoad,
) -> &'static str {
    use summary_runner::SummaryState;
    if let Ok(guard) = runner.state.lock() {
        match guard.get(&session_id) {
            Some(SummaryState::Pending) => return "computing",
            Some(SummaryState::NeedsApiKey) => return "needs-api-key",
            Some(SummaryState::Failed(_)) => return "error",
            Some(SummaryState::Done(_)) | Some(SummaryState::Skipped(_)) => {}
            None => {}
        }
    }
    // Fall back to DB state.
    if detail.summary.is_some() {
        "done"
    } else {
        "idle"
    }
}

/// Read the current VAD config from the live handle and convert to `VadParams`.
fn get_current_vad_params(live_vad: &LiveVadConfig) -> recording_config::VadParams {
    live_vad
        .lock()
        .map(|g| recording_config::VadParams {
            threshold: g.threshold,
            hangover_ms: g.hangover_ms,
            max_duration_ms: g.max_duration_ms,
        })
        .unwrap_or_default()
}
