//! Resampler consumer — reads ring buffer, resamples to 16kHz mono PCM16,
//! emits chunks via mpsc to downstream (Phase 3 VAD, Phase 2 mixer).
//!
//! Plan v2 Section 4.1 — rubato sinc interpolation 48k → 16k.
//! Plan v2 Section 13.1.4 — handle exotic sample rates (e.g., 96 kHz mic).

use crate::error::AudioError;
use crate::types::{TARGET_CHANNELS, TARGET_SAMPLE_RATE_HZ};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use tokio::sync::mpsc;

/// Configuration for the resampler consumer task.
#[derive(Debug, Clone)]
pub struct ResampleConfig {
    /// Source sample rate (Hz) — from cpal device.
    pub source_rate: u32,
    /// Source channel count (1 = mono, 2 = stereo will be downmixed).
    pub source_channels: u16,
    /// Read chunk size from ring buffer (in source samples, interleaved).
    pub read_chunk_size: usize,
}

impl Default for ResampleConfig {
    fn default() -> Self {
        Self {
            source_rate: 48_000,
            source_channels: 2,
            read_chunk_size: 2048,
        }
    }
}

/// Run the resampler consumer task.
///
/// Reads i16 samples from `rx` (the rtrb consumer side), converts to f32,
/// downmixes stereo to mono if needed, resamples to 16 kHz, and sends
/// `Vec<i16>` chunks to `out_tx`.
///
/// Returns when `rx` is closed (producer dropped) OR `out_tx` is closed.
pub async fn run_resampler(
    mut rx: rtrb::Consumer<i16>,
    out_tx: mpsc::Sender<Vec<i16>>,
    cfg: ResampleConfig,
) -> Result<(), AudioError> {
    let ratio = TARGET_SAMPLE_RATE_HZ as f64 / cfg.source_rate as f64;
    let frames_per_chunk = cfg.read_chunk_size / cfg.source_channels.max(1) as usize;

    tracing::info!(
        source_rate = cfg.source_rate,
        source_channels = cfg.source_channels,
        target_rate = TARGET_SAMPLE_RATE_HZ,
        ratio,
        frames_per_chunk,
        "resampler: starting"
    );

    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        oversampling_factor: 256,
        interpolation: SincInterpolationType::Linear,
        window: WindowFunction::BlackmanHarris2,
    };

    let mut resampler = SincFixedIn::<f32>::new(
        ratio,
        /* max_resample_ratio_relative = */ 2.0,
        params,
        frames_per_chunk,
        /* nbr_channels = */ TARGET_CHANNELS as usize, // post-downmix
    )?;

    // Pre-allocated buffers per channel (rubato 0.16 `process_into_buffer` requires
    // output Vecs to have `len()` ≥ `output_frames_max()`, not just capacity).
    // We size to the worst case once and reuse.
    let out_max = resampler.output_frames_max();
    let mut input_buf: Vec<Vec<f32>> =
        vec![Vec::with_capacity(frames_per_chunk); TARGET_CHANNELS as usize];
    let mut output_buf: Vec<Vec<f32>> = vec![vec![0f32; out_max]; TARGET_CHANNELS as usize];

    // Scratch interleaved buffer for ring-buffer read
    let mut scratch_interleaved = vec![0i16; cfg.read_chunk_size];

    loop {
        // Try to read a full chunk from the ring buffer. If insufficient,
        // sleep briefly and retry — this is the producer-pacing strategy.
        match rx.read_chunk(cfg.read_chunk_size) {
            Ok(chunk) => {
                // Copy out of ring (chunk is bounded by ring topology, safe to consume).
                let (first, second) = chunk.as_slices();
                scratch_interleaved[..first.len()].copy_from_slice(first);
                scratch_interleaved[first.len()..first.len() + second.len()]
                    .copy_from_slice(second);
                chunk.commit_all();

                // De-interleave + downmix → mono f32 in [-1.0, 1.0]
                input_buf[0].clear();
                downmix_to_mono_f32(&scratch_interleaved, cfg.source_channels, &mut input_buf[0]);

                // Ask rubato how many output frames this call will produce, then
                // resample. We only quantize the valid prefix — the rest of the
                // pre-sized output_buf is stale (would over-count and add zeros).
                let out_frames = resampler.output_frames_next();
                resampler.process_into_buffer(&input_buf, &mut output_buf, None)?;

                // Quantize f32 → i16 — only the valid prefix
                let mut out_pcm16 = Vec::with_capacity(out_frames);
                for &s in &output_buf[0][..out_frames] {
                    let q = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                    out_pcm16.push(q);
                }

                if out_tx.send(out_pcm16).await.is_err() {
                    tracing::info!("resampler: downstream closed, exiting");
                    return Err(AudioError::ConsumerDropped);
                }
            }
            Err(_) => {
                // Not enough samples yet — yield + retry.
                // 5ms sleep ≈ 240 samples @ 48kHz = below frames_per_chunk.
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
    }
}

/// Downmix interleaved samples to mono f32.
///
/// For stereo: average L+R. For mono: passthrough.
/// Output appended to `out` (caller should `clear()` first).
fn downmix_to_mono_f32(interleaved: &[i16], channels: u16, out: &mut Vec<f32>) {
    let ch = channels.max(1) as usize;
    if ch == 1 {
        for &s in interleaved {
            out.push(s as f32 / i16::MAX as f32);
        }
    } else {
        // Average across channels per frame
        for frame in interleaved.chunks_exact(ch) {
            let sum: i32 = frame.iter().map(|&s| s as i32).sum();
            let avg = sum / ch as i32;
            out.push(avg as f32 / i16::MAX as f32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_mono_passthrough() {
        let input = vec![100i16, 200, 300, 400];
        let mut out = Vec::new();
        downmix_to_mono_f32(&input, 1, &mut out);
        assert_eq!(out.len(), 4);
        assert!((out[0] - (100.0 / i16::MAX as f32)).abs() < 1e-6);
    }

    #[test]
    fn downmix_stereo_averages() {
        // L=100, R=300 → avg=200
        let input = vec![100i16, 300, 50, 150];
        let mut out = Vec::new();
        downmix_to_mono_f32(&input, 2, &mut out);
        assert_eq!(out.len(), 2);
        assert!((out[0] - (200.0 / i16::MAX as f32)).abs() < 1e-6);
        assert!((out[1] - (100.0 / i16::MAX as f32)).abs() < 1e-6);
    }

    #[test]
    fn config_default_is_sane() {
        let c = ResampleConfig::default();
        assert_eq!(c.source_rate, 48_000);
        assert_eq!(c.source_channels, 2);
        assert!(c.read_chunk_size > 0);
    }
}
