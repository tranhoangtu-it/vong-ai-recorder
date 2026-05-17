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
use tokio::sync::mpsc;

/// VAD frame size (samples). Earshot 1.1 fixed at 256 samples @ 16 kHz = 16 ms.
pub const VAD_FRAME_SAMPLES: usize = 256;

/// VAD frame duration in milliseconds.
pub const VAD_FRAME_MS: u32 = 16;

/// Configurable parameters for the VAD finite state machine.
#[derive(Debug, Clone)]
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
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            threshold: 0.5,          // earshot recommends >0.5 as voice
            hangover_ms: 400,        // plan v2 default; spec gốc says 300-500ms
            pre_roll_ms: 200,        // capture "hello" first transient
            min_duration_ms: 200,    // discard <200ms blips
            max_duration_ms: 30_000, // 30s force-pack ceiling
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
    state: VadState,
    pre_roll: PreRollBuffer,
    current: Option<UtteranceBuilder>,
    seq: u64,
}

impl VadFsm {
    /// Construct a new FSM with the given config.
    pub fn new(config: VadConfig) -> Self {
        let hangover_frames_max = config.hangover_ms.div_ceil(VAD_FRAME_MS);
        Self {
            detector: Detector::const_default(),
            pre_roll: PreRollBuffer::from_ms(config.pre_roll_ms),
            hangover_frames_max,
            config,
            state: VadState::Idle,
            current: None,
            seq: 0,
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
            }
            (VadState::Recording { hangover_frames }, true) => {
                // Continued voice → append, reset hangover counter
                if let Some(b) = self.current.as_mut() {
                    b.append_slice(frame);
                }
                *hangover_frames = 0;
            }
            (VadState::Recording { hangover_frames }, false) => {
                // Silence during recording → still record (hangover) but count
                if let Some(b) = self.current.as_mut() {
                    b.append_slice(frame);
                }
                *hangover_frames += 1;

                if *hangover_frames >= self.hangover_frames_max {
                    self.pack(out_tx).await?;
                }
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
        // Hangover frames at default 400ms / 16ms = 25
        assert_eq!(fsm.hangover_frames_max, 25);
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
}
