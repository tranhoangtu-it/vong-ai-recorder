//! Vọng STT — Main entry point.
//!
//! Phase 0: Slint hello window with design tokens preview.
//! Phase 1 + 3 + 4 + 6 (Windows-first, local STT, on-disk history):
//!   cpal WASAPI mic capture → rtrb SPSC ring → rubato sinc resampler
//!   → earshot VAD FSM → Utterance pack → Whisper.cpp local inference
//!   → TranscriptEvent::Final → SQLite FTS5 (NFC-normalized for VN tone search).
//! UI shows live peak meter, utterance counter, latest transcript, and session id.

mod log_init;
mod tray;

use cpal::traits::DeviceTrait;
use rtrb::RingBuffer;
use std::cell::RefCell;
use std::path::PathBuf;
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
use vong_stt::{StreamOpts, StreamingTranscriber, TranscriptEvent, WhisperLocalProvider};

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
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Bind the log-file flush guard for the entire app lifetime — dropping it
    // flushes any pending log lines from the rolling file appender.
    let _log_guard = log_init::init_logging();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Vọng starting");

    let ui = AppWindow::new()?;

    // Audio-source picker: enumerate devices + populate dropdown.
    populate_audio_source_picker(&ui);

    // First-launch onboarding banner. On dismiss, write marker file so it doesn't
    // re-appear next time. Skip the banner entirely if the file already exists.
    if !onboarding_marker_exists() {
        ui.set_onboarding_visible(true);
    }
    ui.on_dismiss_onboarding(move || {
        if let Err(e) = mark_onboarded() {
            tracing::warn!(error = ?e, "failed to write onboarding marker — banner may re-appear");
        } else {
            tracing::info!("onboarding marker written — banner dismissed");
        }
    });

    // Floating Pill — borderless always-on-top overlay alongside main window.
    let pill = FloatingPill::new()?;
    {
        let pill_weak = pill.as_weak();
        pill.on_close_requested(move || {
            if let Some(p) = pill_weak.upgrade() {
                let _ = p.hide();
                tracing::info!("pill: close requested — hiding pill");
            }
        });
    }
    let app_started_at = std::time::Instant::now();

    let audio = match init_audio(&ui, pill.as_weak(), app_started_at) {
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

    pill.show()?;
    ui.run()?;

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
    pill_weak: slint::Weak<FloatingPill>,
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

    // VAD FSM → utterances.
    let (utt_tx, utt_rx) = tokio::sync::mpsc::channel::<Utterance>(32);
    let vad_cfg = VadConfig::default();
    runtime.spawn(async move {
        if let Err(e) = run_vad_fsm(audio_rx, utt_tx, vad_cfg).await {
            tracing::error!(error = ?e, "VAD FSM exited with error");
        } else {
            tracing::info!("VAD FSM exited cleanly");
        }
    });

    let state = Arc::new(PipelineState::default());

    // Open SQLite + create a session row. Storage failure is non-fatal — the
    // pipeline runs without persistence (segments_persisted stays at 0).
    let session = init_storage(&device_name, state.clone());

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

    // Slint timer @ ~30 fps: pull pipeline state into both windows (main + pill).
    // History card is heavier (DB query) — throttled to ~1 Hz via tick counter.
    let ui_weak = ui.as_weak();
    let peak_clone = peak.clone();
    let overflow_clone = overflow.clone();
    let state_ui = state.clone();
    let session_for_timer = session.as_ref().map(SessionContext::clone_for_task);
    let current_query_for_timer = current_query.clone();
    let mut ticks: u32 = 0;
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

            let transcript = state_ui
                .last_transcript
                .lock()
                .map(|s| s.clone())
                .unwrap_or_default();

            let language = state_ui
                .last_language
                .lock()
                .map(|s| {
                    if s.is_empty() {
                        "vi".to_string()
                    } else {
                        s.clone()
                    }
                })
                .unwrap_or_else(|_| "vi".to_string());

            let elapsed = app_started_at.elapsed().as_secs() as i32;

            // Update main window
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_peak_level(level);

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

            // Update floating pill (mirrors live state).
            // During warmup, override transcript with the "starting GPU" hint
            // so user sees something more meaningful than the silent "Hãy nói…".
            if let Some(pill) = pill_weak.upgrade() {
                pill.set_peak_level(level);
                let pill_text = if warming && transcript.is_empty() {
                    "🔥  Đang khởi động GPU…".to_string()
                } else {
                    transcript
                };
                pill.set_last_transcript(pill_text.into());
                pill.set_utterance_count(count as i32);
                pill.set_elapsed_seconds(elapsed);
                pill.set_language_tag(language.into());
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

// ---- Onboarding marker (Phase 7) ------------------------------------------

/// Path to first-launch marker. Existence = user dismissed the welcome banner.
fn onboarding_marker_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "Vong", "Vong")
        .map(|d| d.config_dir().join("onboarded.txt"))
}

fn onboarding_marker_exists() -> bool {
    onboarding_marker_path()
        .map(|p| p.exists())
        .unwrap_or(false)
}

fn mark_onboarded() -> std::io::Result<()> {
    let Some(path) = onboarding_marker_path() else {
        return Err(std::io::Error::other("ProjectDirs unavailable"));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    std::fs::write(&path, format!("{}\n", timestamp))
}

// ---- Audio source picker (Phase 2-W: input + output loopback) -------------

/// Persisted-source config path under `%APPDATA%\Vong\Vong\config\source.txt`.
/// Format: a single line `<kind>|<name>` where kind ∈ {input, loopback}.
fn audio_source_config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "Vong", "Vong").map(|d| d.config_dir().join("source.txt"))
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

/// Wire Whisper into the pipeline: spawn the STT driver + an event consumer
/// that updates `PipelineState` and persists `Final` events into SQLite.
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
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TranscriptEvent>(64);

    // Driver task — runs Whisper inference on each utterance.
    let opts = StreamOpts {
        language_hint: Some("vi".into()),
        enable_lid: false,
        enable_diarization: false,
        enable_translation_to: None,
    };
    runtime.spawn(async move {
        if let Err(e) = provider.transcribe_stream(utt_rx, event_tx, opts).await {
            tracing::error!(error = ?e, "Whisper stream exited with error");
        } else {
            tracing::info!("Whisper stream exited cleanly");
        }
    });

    // Event consumer task — pushes events into shared state + SQLite.
    runtime.spawn(async move {
        while let Some(evt) = event_rx.recv().await {
            match evt {
                TranscriptEvent::Connected => {
                    tracing::info!("STT: connected");
                }
                TranscriptEvent::Final {
                    seq,
                    text,
                    language,
                    start,
                    end,
                    ..
                } => {
                    state.utterance_count.fetch_add(1, Ordering::Relaxed);
                    state.last_seq.store(seq, Ordering::Relaxed);
                    state
                        .last_duration_ms
                        .store(end.as_millis() as u32, Ordering::Relaxed);

                    // Skip empty Whisper output (silence / low-confidence
                    // utterances) when refreshing the UI's last-transcript —
                    // keeps the pill/card showing the last MEANINGFUL line
                    // instead of blanking out. Still persist the empty segment
                    // to keep the seq sequence intact for DB analytics.
                    if !text.trim().is_empty() {
                        if let Ok(mut g) = state.last_transcript.lock() {
                            *g = text.clone();
                        }
                        if let Ok(mut g) = state.last_language.lock() {
                            *g = language.clone().unwrap_or_default();
                        }
                    }

                    // Persist segment if storage is available.
                    if let Some(ref ctx) = session {
                        persist_segment(ctx, seq, start, end, &text, language.as_deref(), &state);
                    }
                }
                TranscriptEvent::Error { code, message } => {
                    tracing::warn!(code, message, "STT: recoverable error");
                }
                TranscriptEvent::Disconnected => {
                    tracing::info!("STT: disconnected");
                }
                _ => {}
            }
        }
        tracing::info!("STT event consumer exited (channel closed)");
    });
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
