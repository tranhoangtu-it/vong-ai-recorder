//! Voice Activity Detection — earshot wrapper + finite state machine.
//!
//! Plan v2 Section 4.1 — earshot pure Rust VAD (pykeio), RTF ~0.0007.
//! Plan v2 Section 13.1.5 — FSM IDLE → RECORDING → HANGOVER → PACK.
//! Spec gốc dòng 84 — hangover 300-500ms default.
//!
//! Frame size: **256 samples @ 16 kHz = 16 ms** (earshot 1.1 fixed).
//!
//! # Architecture
//!
//! ```text
//!  mpsc<Vec<i16>> (16k mono PCM) from resampler (Phase 1) / mixer (Phase 2)
//!         │
//!         ▼
//!   VadFsm::process_chunk()
//!         │   For each 256-sample frame:
//!         │     score = Detector::predict_i16(frame)
//!         │
//!         ▼
//!   State transitions:
//!     IDLE      + score ≥ thr → RECORDING  (drain pre-roll, append frame)
//!     RECORDING + score ≥ thr → RECORDING  (append, reset hangover counter)
//!     RECORDING + score <  thr → HANGOVER  (append, ++counter)
//!     HANGOVER  + counter ≥ N  → PACK      (build Utterance, send via mpsc, → IDLE)
//!         │
//!         ▼
//!   mpsc<Utterance> to Phase 4 Soniox provider
//! ```

use crate::error::AudioError;
use crate::types::TARGET_SAMPLE_RATE_HZ;
use crate::utterance::{PreRollBuffer, Utterance, UtteranceBuilder};
use earshot::{DefaultPredictor, Detector};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Shared handle for live VAD config mutation from the UI thread.
///
/// Mirror of `LiveConfigHandle` in `vong-transcribe`. The VAD FSM snapshots
/// this once per `process_chunk` call (~16 ms cadence). Lock is held only
/// for the duration of a small struct clone — contention is negligible.
pub type LiveVadConfig = Arc<Mutex<VadConfig>>;

/// VAD frame size (samples). Earshot 1.1 fixed at 256 samples @ 16 kHz = 16 ms.
pub const VAD_FRAME_SAMPLES: usize = 256;

/// VAD frame duration in milliseconds.
pub const VAD_FRAME_MS: u32 = 16;

/// Configurable parameters for the VAD finite state machine.
///
/// Implements `Serialize`/`Deserialize` for persistence in `recording.json`.
/// The `partial_emit_ms` field is skipped during serialization — it is a
/// protocol constant coupled to earshot's 256-sample tokenization and must
/// never be exposed as a user-adjustable slider (changing it mid-stream
/// while `frames_since_partial` is non-zero would skip/repeat a partial
/// emission). Only `threshold`, `hangover_ms`, and `max_duration_ms` are
/// user-tunable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VadConfig {
    /// Voice score threshold in `[0.0, 1.0]`. Frames at or above are voiced.
    pub threshold: f32,
    /// Hangover duration in milliseconds. Hold recording state this long
    /// after VAD drops below threshold before packing the utterance.
    pub hangover_ms: u32,
    /// Pre-roll capture in milliseconds. Audio retained before voice start.
    pub pre_roll_ms: u32,
    /// Minimum utterance duration; shorter packed segments are discarded.
    pub min_duration_ms: u32,
    /// Maximum utterance duration; force-pack at this length to avoid runaway.
    pub max_duration_ms: u32,
    /// While in the RECORDING state, emit a Partial `Utterance` snapshot
    /// every `partial_emit_ms` of accumulated audio. Drives the real-time
    /// "Bản gốc" column. `0` disables partials (fall back to per-utterance only).
    ///
    /// Default 1500 ms — empirically a good balance:
    /// - too short (<800 ms) → snapshot clone overhead + Whisper backlog
    /// - too long (>2500 ms) → "đến đâu hiện đến đấy" no longer feels live
    ///
    /// **NOT serialized**: this field is a protocol constant tied to earshot's
    /// 256-sample tokenization. Changing it mid-stream while `frames_since_partial`
    /// is non-zero would skip or repeat a partial emission. The deserialize default
    /// restores the correct value automatically.
    #[serde(skip, default = "default_partial_emit_ms")]
    pub partial_emit_ms: u32,
}

/// Default value for `partial_emit_ms` when deserializing from JSON (field is
/// skipped, so serde needs this function to supply the correct protocol value).
fn default_partial_emit_ms() -> u32 {
    1_500
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            // Higher threshold so ambient/noise/music isn't classified as
            // voice — packs only when the user actually speaks. Was 0.5
            // (earshot recommended floor); 0.65 noticeably reduces
            // false-positive "RECORDING" when Speakers loopback is the
            // capture source and the system is playing background audio.
            threshold: 0.65,
            // Shorter hangover so a natural sentence pause closes the
            // utterance and pack-fires quickly. Was 400 ms; 200 ms still
            // catches the tail of plosive sounds while feeling snappy.
            hangover_ms: 200,
            pre_roll_ms: 200,        // capture "hello" first transient
            min_duration_ms: 200,    // discard <200ms blips
            // Hard ceiling lowered from 30 s → 8 s. On noisy loopback the
            // FSM can stay in RECORDING for the full ceiling without
            // detecting a clean silence; capping at 8 s means UI sees a
            // Final pack within ~8 s in the worst case, instead of waiting
            // half a minute. Long sentences split into multiple utterances
            // is an acceptable trade-off for real-time UX.
            max_duration_ms: 8_000,
            partial_emit_ms: 1_500,
        }
    }
}

/// FSM state.
#[derive(Debug)]
enum VadState {
    Idle,
    Recording { hangover_frames: u32 },
}

/// Stateful VAD finite state machine — owns Detector + buffers, processes
/// incoming PCM chunks, emits `Utterance` via mpsc on pack.
pub struct VadFsm {
    detector: Detector<DefaultPredictor>,
    config: VadConfig,
    hangover_frames_max: u32,
    /// Frames between partial-emit snapshots (derived from `partial_emit_ms`).
    /// 0 disables partials entirely.
    partial_emit_frames: u32,
    /// Frame counter since last partial snapshot (only meaningful while in
    /// Recording state).
    frames_since_partial: u32,
    state: VadState,
    pre_roll: PreRollBuffer,
    current: Option<UtteranceBuilder>,
    seq: u64,
    /// Optional live config handle. When present, `maybe_refresh_config` is
    /// called at the top of each `process_chunk` to pick up UI slider changes
    /// without restarting the FSM.
    live_handle: Option<LiveVadConfig>,
}

impl VadFsm {
    /// Construct a new FSM with the given config.
    pub fn new(config: VadConfig) -> Self {
        let hangover_frames_max = config.hangover_ms.div_ceil(VAD_FRAME_MS);
        let partial_emit_frames = if config.partial_emit_ms == 0 {
            0
        } else {
            config.partial_emit_ms.div_ceil(VAD_FRAME_MS)
        };
        Self {
            detector: Detector::const_default(),
            pre_roll: PreRollBuffer::from_ms(config.pre_roll_ms),
            hangover_frames_max,
            partial_emit_frames,
            frames_since_partial: 0,
            config,
            state: VadState::Idle,
            current: None,
            seq: 0,
            live_handle: None,
        }
    }

    /// Construct a FSM that reads config updates from a shared live handle.
    ///
    /// The handle is cloned into the FSM; UI thread and FSM both hold an
    /// `Arc<Mutex<VadConfig>>`. The FSM snapshots the config at the top of
    /// each `process_chunk` call. Threshold / hangover / max_duration changes
    /// take effect on the very next VAD frame (~16 ms latency).
    pub fn new_with_live(handle: LiveVadConfig) -> Self {
        let initial = handle.lock().expect("live VAD config poisoned").clone();
        let mut fsm = Self::new(initial);
        fsm.live_handle = Some(handle);
        fsm
    }

    /// Snapshot the live handle (if any) and apply updated values.
    ///
    /// Called at the top of each `process_chunk` — executes a mutex lock +
    /// struct clone (~20 bytes) every ~16 ms. Contention is negligible because
    /// the UI only writes on slider drag events.
    ///
    /// `partial_emit_frames` is intentionally NOT recomputed here — it is a
    /// protocol constant (partial_emit_ms = 1500 ms fixed). Recomputing mid-
    /// stream while `frames_since_partial` is non-zero would skip or repeat a
    /// partial emission.
    fn maybe_refresh_config(&mut self) {
        let Some(ref handle) = self.live_handle else {
            return;
        };
        let snapshot = handle.lock().expect("live VAD config poisoned").clone();
        if snapshot.threshold != self.config.threshold
            || snapshot.hangover_ms != self.config.hangover_ms
            || snapshot.max_duration_ms != self.config.max_duration_ms
        {
            tracing::info!(
                threshold = snapshot.threshold,
                hangover_ms = snapshot.hangover_ms,
                max_duration_ms = snapshot.max_duration_ms,
                "VAD: live config update applied"
            );
            self.hangover_frames_max = snapshot.hangover_ms.div_ceil(VAD_FRAME_MS);
            self.config = snapshot;
            // partial_emit_frames intentionally NOT recomputed — see doc above.
        }
    }

    /// Process a chunk of 16 kHz mono PCM16 samples. May produce zero, one,
    /// or multiple `Utterance` outputs as VAD detects speech segments.
    ///
    /// Sends each finalized utterance via `out_tx`. Returns `Err(ConsumerDropped)`
    /// if the downstream channel is closed.
    pub async fn process_chunk(
        &mut self,
        chunk: &[i16],
        out_tx: &mpsc::Sender<Utterance>,
    ) -> Result<(), AudioError> {
        // Snapshot live config at the start of each chunk (~16 ms cadence).
        // Cheap: mutex lock + small struct clone. No effect when live_handle is None.
        self.maybe_refresh_config();

        for frame in chunk.chunks_exact(VAD_FRAME_SAMPLES) {
            let score = self.detector.predict_i16(frame);
            let voiced = score >= self.config.threshold;
            self.step(frame, voiced, out_tx).await?;
        }
        Ok(())
    }

    /// Reset internal state (call on stream change / restart).
    pub fn reset(&mut self) {
        self.detector.reset();
        self.state = VadState::Idle;
        self.current = None;
        let _ = self.pre_roll.drain_to_vec(); // clear
    }

    /// One frame step of the FSM.
    async fn step(
        &mut self,
        frame: &[i16],
        voiced: bool,
        out_tx: &mpsc::Sender<Utterance>,
    ) -> Result<(), AudioError> {
        match (&mut self.state, voiced) {
            (VadState::Idle, false) => {
                // Silence in idle → just collect into pre-roll
                self.pre_roll.push(frame);
            }
            (VadState::Idle, true) => {
                // Voice onset → transition to RECORDING, prepend pre-roll
                let onset_seq = self.seq;
                let mut builder = UtteranceBuilder::new(onset_seq);
                self.seq = self.seq.wrapping_add(1);
                let pre = self.pre_roll.drain_to_vec();
                builder.append_slice(&pre);
                builder.append_slice(frame);
                tracing::info!(
                    seq = onset_seq,
                    pre_roll_samples = pre.len(),
                    initial_ms = builder.duration_ms(),
                    "VAD: utterance start"
                );
                self.current = Some(builder);
                self.state = VadState::Recording { hangover_frames: 0 };
                self.frames_since_partial = 0;
            }
            (VadState::Recording { hangover_frames }, true) => {
                // Continued voice → append, reset hangover counter
                if let Some(b) = self.current.as_mut() {
                    b.append_slice(frame);
                }
                *hangover_frames = 0;
                self.frames_since_partial = self.frames_since_partial.saturating_add(1);
            }
            (VadState::Recording { hangover_frames }, false) => {
                // Silence during recording → still record (hangover) but count
                if let Some(b) = self.current.as_mut() {
                    b.append_slice(frame);
                }
                *hangover_frames += 1;
                self.frames_since_partial = self.frames_since_partial.saturating_add(1);

                if *hangover_frames >= self.hangover_frames_max {
                    self.pack(out_tx).await?;
                }
            }
        }

        // Emit a Partial snapshot if it's time. Runs only in Recording state;
        // Idle / Hangover-just-packed states reset frames_since_partial = 0.
        if let VadState::Recording { .. } = self.state {
            if self.partial_emit_frames > 0
                && self.frames_since_partial >= self.partial_emit_frames
            {
                if let Some(b) = self.current.as_ref() {
                    let partial = b.snapshot_partial();
                    tracing::debug!(
                        seq = partial.seq,
                        duration_ms = partial.duration_ms,
                        samples = partial.sample_count(),
                        "VAD: emit partial"
                    );
                    out_tx
                        .send(partial)
                        .await
                        .map_err(|_| AudioError::ConsumerDropped)?;
                }
                self.frames_since_partial = 0;
            }
        }

        // Force-pack if utterance hits max duration
        if let VadState::Recording { .. } = self.state {
            if let Some(b) = self.current.as_ref() {
                if b.duration_ms() >= self.config.max_duration_ms {
                    tracing::warn!(
                        max_ms = self.config.max_duration_ms,
                        "VAD: max duration reached, force-packing"
                    );
                    self.pack(out_tx).await?;
                }
            }
        }
        Ok(())
    }

    /// Finalize current utterance and emit. Returns to IDLE state.
    async fn pack(&mut self, out_tx: &mpsc::Sender<Utterance>) -> Result<(), AudioError> {
        if let Some(builder) = self.current.take() {
            let dur_ms = builder.duration_ms();
            if dur_ms < self.config.min_duration_ms {
                tracing::debug!(
                    duration_ms = dur_ms,
                    min_ms = self.config.min_duration_ms,
                    "VAD: discard short utterance"
                );
            } else {
                let utt = builder.build();
                tracing::info!(
                    seq = utt.seq,
                    duration_ms = utt.duration_ms,
                    sample_count = utt.sample_count(),
                    "VAD: utterance pack"
                );
                out_tx
                    .send(utt)
                    .await
                    .map_err(|_| AudioError::ConsumerDropped)?;
            }
        }
        self.state = VadState::Idle;
        self.frames_since_partial = 0;
        Ok(())
    }
}

/// Spawn a tokio task running the VAD FSM. Convenience wrapper.
///
/// Returns when `audio_rx` is closed (producer dropped) or `out_tx` closed.
pub async fn run_vad_fsm(
    mut audio_rx: mpsc::Receiver<Vec<i16>>,
    out_tx: mpsc::Sender<Utterance>,
    config: VadConfig,
) -> Result<(), AudioError> {
    let mut fsm = VadFsm::new(config);
    tracing::info!(
        sample_rate = TARGET_SAMPLE_RATE_HZ,
        frame_samples = VAD_FRAME_SAMPLES,
        frame_ms = VAD_FRAME_MS,
        "VAD FSM started"
    );

    while let Some(chunk) = audio_rx.recv().await {
        fsm.process_chunk(&chunk, &out_tx).await?;
    }
    tracing::info!("VAD FSM stopped (audio source closed)");
    Ok(())
}

/// Variant of `run_vad_fsm` that accepts a `LiveVadConfig` handle so the UI
/// can mutate threshold / hangover_ms / max_duration_ms at runtime without
/// restarting the FSM or the audio pipeline.
///
/// Keeps the original `run_vad_fsm` for backward compatibility — existing
/// tests and any static callers that don't need live config still work.
pub async fn run_vad_fsm_live(
    mut audio_rx: mpsc::Receiver<Vec<i16>>,
    out_tx: mpsc::Sender<Utterance>,
    handle: LiveVadConfig,
) -> Result<(), AudioError> {
    let mut fsm = VadFsm::new_with_live(handle);
    tracing::info!(
        sample_rate = TARGET_SAMPLE_RATE_HZ,
        frame_samples = VAD_FRAME_SAMPLES,
        frame_ms = VAD_FRAME_MS,
        "VAD FSM (live config) started"
    );

    while let Some(chunk) = audio_rx.recv().await {
        fsm.process_chunk(&chunk, &out_tx).await?;
    }
    tracing::info!("VAD FSM stopped (audio source closed)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vad_config_default_sane() {
        let c = VadConfig::default();
        assert!(c.threshold > 0.0 && c.threshold < 1.0);
        assert!(c.hangover_ms >= 200 && c.hangover_ms <= 1000);
        assert!(c.pre_roll_ms > 0);
        assert!(c.min_duration_ms < c.max_duration_ms);
    }

    #[test]
    fn fsm_construction() {
        let fsm = VadFsm::new(VadConfig::default());
        assert_eq!(fsm.seq, 0);
        // Hangover frames at default 200ms / 16ms = 12.5 → ceil 13
        assert_eq!(fsm.hangover_frames_max, 13);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fsm_silence_stays_idle() {
        let mut fsm = VadFsm::new(VadConfig::default());
        let (tx, mut rx) = mpsc::channel(8);

        // 10 frames of silence
        let silence = vec![0i16; VAD_FRAME_SAMPLES * 10];
        fsm.process_chunk(&silence, &tx).await.unwrap();

        // Should emit zero utterances
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fsm_processes_chunks_without_panic() {
        // Even with random-looking samples, FSM should not panic
        let mut fsm = VadFsm::new(VadConfig::default());
        let (tx, _rx) = mpsc::channel(8);

        let mut samples = Vec::with_capacity(VAD_FRAME_SAMPLES * 5);
        for i in 0..VAD_FRAME_SAMPLES * 5 {
            samples.push((i as i16).wrapping_mul(127));
        }
        fsm.process_chunk(&samples, &tx).await.unwrap();
    }

    #[test]
    fn frame_size_matches_earshot_spec() {
        // earshot 1.1 fixed: 256 samples @ 16kHz = 16ms
        assert_eq!(VAD_FRAME_SAMPLES, 256);
        assert_eq!(VAD_FRAME_MS, 16);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn live_config_update_applies_on_next_chunk() {
        // Construct FSM with a live handle at default threshold (0.65).
        let handle: LiveVadConfig = Arc::new(Mutex::new(VadConfig::default()));
        let mut fsm = VadFsm::new_with_live(handle.clone());

        // Verify initial config snapshot in the FSM.
        assert!((fsm.config.threshold - 0.65).abs() < f32::EPSILON);

        // Mutate through the handle (simulates UI slider drag).
        handle.lock().unwrap().threshold = 0.90;

        // Send one chunk of silence to trigger process_chunk → maybe_refresh_config.
        let (tx, _rx) = mpsc::channel(8);
        let silence = vec![0i16; VAD_FRAME_SAMPLES * 2];
        fsm.process_chunk(&silence, &tx).await.unwrap();

        // FSM's internal config must now reflect the new threshold.
        assert!(
            (fsm.config.threshold - 0.90).abs() < f32::EPSILON,
            "expected threshold 0.90, got {}",
            fsm.config.threshold
        );
    }

    #[test]
    fn live_config_no_panic_on_poison() {
        // Verify that constructing with a live handle and reading it doesn't panic
        // under normal usage (not testing actual poison which requires a panic).
        let handle: LiveVadConfig = Arc::new(Mutex::new(VadConfig::default()));
        let fsm = VadFsm::new_with_live(handle.clone());
        assert!((fsm.config.threshold - 0.65).abs() < f32::EPSILON);

        // Mutate the hangover_ms and verify it's visible through the handle.
        handle.lock().unwrap().hangover_ms = 400;
        assert_eq!(handle.lock().unwrap().hangover_ms, 400);
    }

    #[test]
    fn partial_emit_ms_not_in_serde_output() {
        // partial_emit_ms is skipped during serialization (protocol constant).
        // Verify that round-tripping via serde_json restores the default value.
        let cfg = VadConfig {
            threshold: 0.7,
            hangover_ms: 300,
            max_duration_ms: 5000,
            pre_roll_ms: 100,
            min_duration_ms: 100,
            partial_emit_ms: 9999, // intentionally set to non-default
        };
        let json = serde_json::to_string(&cfg).unwrap();
        // partial_emit_ms must not appear in the JSON output
        assert!(!json.contains("partial_emit_ms"), "partial_emit_ms should be skipped: {}", json);

        // On deserialization, partial_emit_ms must come back as the protocol default (1500).
        let restored: VadConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored.partial_emit_ms,
            1500,
            "partial_emit_ms must restore to 1500 after roundtrip"
        );
        assert!((restored.threshold - 0.7).abs() < f32::EPSILON);
    }
}
