//! Core audio types — config, peak meter, audio sources.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// Target output configuration for all STT providers (Whisper, Soniox, etc.).
/// PCM signed 16-bit, mono, 16 kHz per plan v2 Section 4.1.
pub const TARGET_SAMPLE_RATE_HZ: u32 = 16_000;

/// Target channels (mono).
pub const TARGET_CHANNELS: u16 = 1;

/// Max single audio callback block size (samples).
/// Bound prevents callback stack allocation overflow.
/// 4096 samples @ 48kHz stereo ≈ 42ms — generous safety margin.
pub const MAX_CALLBACK_BLOCK: usize = 4096;

/// Audio source identifier (used by Phase 2 mixer to label streams).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSource {
    /// Microphone input via cpal default input device.
    Microphone,
    /// macOS system audio output via ScreenCaptureKit (Phase 2).
    LoopbackMacOs,
    /// Mixed: mic + loopback combined post-clock-drift (Phase 2).
    Mixed,
}

impl AudioSource {
    /// Stable string identifier for logs/storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Microphone => "mic",
            Self::LoopbackMacOs => "loopback",
            Self::Mixed => "mixed",
        }
    }
}

/// Active stream configuration negotiated with the device.
#[derive(Debug, Clone)]
pub struct AudioConfig {
    /// Device-reported sample rate (Hz). Typically 48000 or 44100.
    pub sample_rate: u32,
    /// Channel count (1 = mono, 2 = stereo).
    pub channels: u16,
    /// Suggested buffer size in samples per callback.
    pub buffer_size: u32,
}

impl AudioConfig {
    /// Estimate per-callback duration in milliseconds.
    pub fn callback_duration_ms(&self) -> f32 {
        (self.buffer_size as f32 / self.sample_rate as f32) * 1000.0
    }
}

/// Real-time peak amplitude meter (atomic, lock-free).
///
/// Audio callback `update()` from RT thread; UI calls `read_and_reset()` at 30 fps
/// for level meter rendering. Stores normalized abs amplitude `[0.0, 1.0]` as `u32` bits.
#[derive(Debug, Clone, Default)]
pub struct PeakMeter(Arc<AtomicU32>);

impl PeakMeter {
    /// Create a new peak meter starting at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Update with new peak (called from RT audio callback, never blocks).
    ///
    /// `peak` is normalized amplitude in `[0.0, 1.0]`. Negative or out-of-range
    /// values are clamped on read.
    #[inline]
    pub fn update(&self, peak: f32) {
        // Encode |peak| as u32. `fetch_max` is lock-free, RT-safe.
        let bits = (peak.abs().min(1.0) * u32::MAX as f32) as u32;
        self.0.fetch_max(bits, Ordering::Relaxed);
    }

    /// Read current peak and reset to zero atomically. UI calls at refresh rate.
    pub fn read_and_reset(&self) -> f32 {
        let bits = self.0.swap(0, Ordering::Relaxed);
        bits as f32 / u32::MAX as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_meter_records_max() {
        let m = PeakMeter::new();
        m.update(0.3);
        m.update(0.7);
        m.update(0.5);
        let v = m.read_and_reset();
        assert!((v - 0.7).abs() < 0.001, "expected ~0.7, got {v}");
    }

    #[test]
    fn peak_meter_resets_on_read() {
        let m = PeakMeter::new();
        m.update(0.5);
        let _ = m.read_and_reset();
        let v = m.read_and_reset();
        assert_eq!(v, 0.0);
    }

    #[test]
    fn peak_meter_clamps_out_of_range() {
        let m = PeakMeter::new();
        m.update(1.5); // > 1.0 should be clamped
        let v = m.read_and_reset();
        assert!(v <= 1.0);
    }

    #[test]
    fn audio_source_str_stable() {
        assert_eq!(AudioSource::Microphone.as_str(), "mic");
        assert_eq!(AudioSource::LoopbackMacOs.as_str(), "loopback");
        assert_eq!(AudioSource::Mixed.as_str(), "mixed");
    }

    #[test]
    fn callback_duration_calculation() {
        let cfg = AudioConfig {
            sample_rate: 48_000,
            channels: 2,
            buffer_size: 1024,
        };
        let ms = cfg.callback_duration_ms();
        assert!((ms - 21.333).abs() < 0.01, "expected ~21.33ms, got {ms}");
    }
}
