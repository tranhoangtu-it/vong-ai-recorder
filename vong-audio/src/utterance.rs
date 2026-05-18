//! Utterance — discrete voice-bounded audio segment + pre-roll capture buffer.
//!
//! Plan v2 Section 4.2 — Utterances flow from VAD into STT providers.
//! Phase 4 (Soniox WebSocket) consumes `mpsc<Utterance>` from VAD FSM.

use std::collections::VecDeque;
use std::time::SystemTime;
use uuid::Uuid;

use crate::types::TARGET_SAMPLE_RATE_HZ;

/// A voice-bounded audio segment ready for STT processing.
///
/// Payload is 16 kHz mono PCM16 (matches STT provider expectations,
/// see plan v2 Section 4.1).
///
/// Two flavors:
/// - **Final** (`is_partial = false`) — VAD has packed the complete utterance
///   after hangover or max-duration. Suitable for translation + DB persist.
/// - **Partial** (`is_partial = true`) — snapshot mid-utterance for real-time
///   "Bản gốc" UI feedback. Same `seq` as the eventual final, identified by
///   the flag. Should run only fast inference (no translation pass, no persist).
#[derive(Debug, Clone)]
pub struct Utterance {
    /// Stable unique identifier (UUID v4) — fresh per snapshot.
    pub id: Uuid,

    /// Monotonic sequence number within the recording session. Partials and
    /// the final for the same speech segment share the same `seq`.
    pub seq: u64,

    /// Recording wall-clock start time.
    pub started_at: SystemTime,

    /// Duration in milliseconds (derived from sample count).
    pub duration_ms: u32,

    /// 16 kHz mono PCM16 samples. Includes pre-roll (~200ms) + speech + hangover.
    pub audio_pcm16_mono_16k: Vec<i16>,

    /// True for periodic mid-speech snapshots. Consumers use this to decide
    /// fast-only vs full inference.
    pub is_partial: bool,
}

impl Utterance {
    /// Sample count of the underlying audio.
    pub fn sample_count(&self) -> usize {
        self.audio_pcm16_mono_16k.len()
    }
}

/// Builder for an in-progress utterance (only finalized when VAD FSM packs).
pub struct UtteranceBuilder {
    seq: u64,
    started_at: SystemTime,
    samples: Vec<i16>,
}

impl UtteranceBuilder {
    /// Start a new utterance with the given sequence number.
    pub fn new(seq: u64) -> Self {
        Self {
            seq,
            started_at: SystemTime::now(),
            samples: Vec::with_capacity(TARGET_SAMPLE_RATE_HZ as usize * 5), // 5s headroom
        }
    }

    /// Append a slice of PCM16 samples (16 kHz mono).
    pub fn append_slice(&mut self, frames: &[i16]) {
        self.samples.extend_from_slice(frames);
    }

    /// Current duration in milliseconds based on accumulated samples.
    pub fn duration_ms(&self) -> u32 {
        ((self.samples.len() as u64 * 1000) / TARGET_SAMPLE_RATE_HZ as u64) as u32
    }

    /// Current sample count.
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// Finalize the builder into an immutable Final `Utterance`. Consumes
    /// the builder.
    pub fn build(self) -> Utterance {
        let duration_ms =
            ((self.samples.len() as u64 * 1000) / TARGET_SAMPLE_RATE_HZ as u64) as u32;
        Utterance {
            id: Uuid::new_v4(),
            seq: self.seq,
            started_at: self.started_at,
            duration_ms,
            audio_pcm16_mono_16k: self.samples,
            is_partial: false,
        }
    }

    /// Snapshot the in-progress audio as a Partial `Utterance` without
    /// consuming the builder. Used by VAD to push periodic real-time updates
    /// while still accumulating new frames for the eventual Final.
    ///
    /// Cost: clones the current `samples` Vec. At 16 kHz mono i16 a few seconds
    /// of speech is ~32 KB per clone — well under the 5 ms audio-callback
    /// budget. Don't lower the partial-emit interval to single-frame frequency
    /// or this becomes a hot spot.
    pub fn snapshot_partial(&self) -> Utterance {
        let duration_ms =
            ((self.samples.len() as u64 * 1000) / TARGET_SAMPLE_RATE_HZ as u64) as u32;
        Utterance {
            id: Uuid::new_v4(),
            seq: self.seq,
            started_at: self.started_at,
            duration_ms,
            audio_pcm16_mono_16k: self.samples.clone(),
            is_partial: true,
        }
    }
}

/// Rolling buffer that retains the most-recent N samples, used to prepend
/// `pre_roll_ms` audio onto an utterance when VAD detects voice start.
///
/// Plan v2 Section 13.1.5 / Section 4.4 — captures first-word transient
/// (e.g., "ello" → "hello") that VAD threshold would otherwise miss.
pub struct PreRollBuffer {
    buf: VecDeque<i16>,
    capacity: usize,
}

impl PreRollBuffer {
    /// Create with given capacity in samples.
    ///
    /// E.g., `200 ms` at 16 kHz = 3200 samples.
    pub fn new(capacity_samples: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity_samples),
            capacity: capacity_samples,
        }
    }

    /// Convenience: create with capacity in milliseconds @ 16 kHz.
    pub fn from_ms(ms: u32) -> Self {
        Self::new((ms as usize * TARGET_SAMPLE_RATE_HZ as usize) / 1000)
    }

    /// Push samples; oldest are evicted if capacity exceeded.
    pub fn push(&mut self, samples: &[i16]) {
        for &s in samples {
            if self.buf.len() == self.capacity {
                self.buf.pop_front();
            }
            self.buf.push_back(s);
        }
    }

    /// Drain all buffered samples into a contiguous `Vec`.
    pub fn drain_to_vec(&mut self) -> Vec<i16> {
        self.buf.drain(..).collect()
    }

    /// Current sample count.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// True if empty.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utterance_builder_tracks_duration() {
        let mut b = UtteranceBuilder::new(0);
        // 16000 samples @ 16kHz = 1000ms
        b.append_slice(&vec![0i16; 16_000]);
        assert_eq!(b.duration_ms(), 1000);
        assert_eq!(b.sample_count(), 16_000);
    }

    #[test]
    fn utterance_build_assigns_unique_ids() {
        let u1 = UtteranceBuilder::new(0).build();
        let u2 = UtteranceBuilder::new(0).build();
        assert_ne!(u1.id, u2.id);
    }

    #[test]
    fn preroll_buffer_evicts_oldest() {
        let mut p = PreRollBuffer::new(4);
        p.push(&[1, 2, 3]);
        p.push(&[4, 5]); // pushes 4, 5 — evicts 1
        assert_eq!(p.len(), 4);
        let v = p.drain_to_vec();
        assert_eq!(v, vec![2, 3, 4, 5]);
    }

    #[test]
    fn preroll_from_ms() {
        // 200ms @ 16kHz = 3200 samples
        let p = PreRollBuffer::from_ms(200);
        assert_eq!(p.capacity, 3200);
    }

    #[test]
    fn preroll_drain_empties() {
        let mut p = PreRollBuffer::new(8);
        p.push(&[1, 2, 3]);
        let _ = p.drain_to_vec();
        assert!(p.is_empty());
    }
}
