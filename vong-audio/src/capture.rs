//! Microphone capture via cpal — RT-priority thread + lock-free ring buffer.
//!
//! Plan v2 Section 13.1.1 — promote audio thread to real-time priority.
//! Plan v2 Section 13.1.2 — callback must NOT alloc, lock, syscall, or drop.
//!
//! Architecture (see plan v2 Section 4.2 threading diagram):
//!
//! ```text
//!   cpal callback (RT-promoted)
//!         │ f32 samples (device-native rate, possibly stereo)
//!         ▼
//!   quantize f32 → i16 on stack array (no alloc)
//!         │
//!         ▼
//!   rtrb::Producer<i16>::push_slice (lock-free SPSC)
//!         │
//!   [Phase 1 consumer: resampler.rs reads here]
//! ```

use crate::device::{negotiate_config, to_stream_config, DeviceKind};
use crate::error::AudioError;
use crate::types::{AudioConfig, OverflowCounter, PeakMeter, MAX_CALLBACK_BLOCK};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};
use std::sync::atomic::{AtomicBool, Ordering};

/// Capture stream handle. Dropping stops capture.
///
/// `Stream` is `!Send` on most platforms (carries platform-specific resources),
/// so this handle stays on the thread that built it. Consumer reads from
/// `rtrb::Consumer<i16>` returned alongside via the ring buffer setup.
pub struct CaptureHandle {
    _stream: Stream,
    config: AudioConfig,
}

impl CaptureHandle {
    /// Negotiated stream config (sample rate, channels, buffer size).
    pub fn config(&self) -> &AudioConfig {
        &self.config
    }
}

/// Build + start mic capture stream feeding into an `rtrb` ring buffer.
///
/// # Arguments
/// - `device`: the cpal input device (typically `default_input_device()`).
/// - `tx`: ring buffer producer (i16 samples, interleaved if stereo).
/// - `peak`: shared atomic peak meter for UI level rendering.
///
/// # Lifetime
/// Stream stays alive while `CaptureHandle` is held. Drop the handle to stop.
///
/// # Safety
/// The cpal callback runs on a real-time thread. We promote it on first
/// invocation. Inside the callback we:
/// - Do NOT allocate (stack arrays only, bounded by `MAX_CALLBACK_BLOCK`).
/// - Do NOT lock (atomics + lock-free ring only).
/// - Do NOT syscall via logging (only atomic counters; error counter checked
///   from consumer thread).
#[allow(deprecated)] // cpal 0.17: name() deprecated, but our usage is for display only
pub fn start_capture(
    device: &Device,
    kind: DeviceKind,
    mut tx: rtrb::Producer<i16>,
    peak: PeakMeter,
    overflow: OverflowCounter,
) -> Result<CaptureHandle, AudioError> {
    let supported = negotiate_config(device, kind)?;
    let sample_format = supported.sample_format();
    let channels = supported.channels();
    let sample_rate = supported.sample_rate();
    let stream_config: StreamConfig = to_stream_config(&supported);

    let device_name = device.name().unwrap_or_else(|_| "unknown".into());
    tracing::info!(
        device = %device_name,
        sample_format = ?sample_format,
        sample_rate,
        channels,
        "cpal: building input stream"
    );

    // RT-promotion happens once on first callback (idempotent via AtomicBool).
    // We share this flag across closures of multiple sample formats via static.
    static RT_PROMOTED: AtomicBool = AtomicBool::new(false);

    let err_fn = |err: cpal::StreamError| {
        // RT-thread safe: tracing::error has lock-free fast-path when no subscriber
        // is filtering at error level. We accept rare allocation here since stream
        // errors are exceptional, not steady-state.
        tracing::error!(error = %err, "cpal stream error");
    };

    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _info: &cpal::InputCallbackInfo| {
                promote_rt_once(&RT_PROMOTED, sample_rate);
                forward_f32(data, channels, &mut tx, &peak, &overflow);
            },
            err_fn,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _info: &cpal::InputCallbackInfo| {
                promote_rt_once(&RT_PROMOTED, sample_rate);
                forward_i16(data, channels, &mut tx, &peak, &overflow);
            },
            err_fn,
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            move |data: &[u16], _info: &cpal::InputCallbackInfo| {
                promote_rt_once(&RT_PROMOTED, sample_rate);
                forward_u16(data, channels, &mut tx, &peak, &overflow);
            },
            err_fn,
            None,
        )?,
        SampleFormat::U8 => device.build_input_stream(
            &stream_config,
            move |data: &[u8], _info: &cpal::InputCallbackInfo| {
                promote_rt_once(&RT_PROMOTED, sample_rate);
                forward_u8(data, channels, &mut tx, &peak, &overflow);
            },
            err_fn,
            None,
        )?,
        other => {
            return Err(AudioError::UnsupportedConfig {
                device: device_name,
                requested_hz: sample_rate,
            })
            .inspect_err(|_| {
                tracing::error!(?other, "cpal: unsupported sample format");
            });
        }
    };

    stream.play()?;
    tracing::info!(sample_rate, channels, "cpal: capture started");

    Ok(CaptureHandle {
        _stream: stream,
        config: AudioConfig {
            sample_rate,
            channels,
            buffer_size: 0, // cpal default — exact size determined per callback
        },
    })
}

/// Promote current thread to real-time once. No-op on subsequent calls.
///
/// Failure is logged but non-fatal — capture continues with default priority
/// (Section 13.1.1 plan v2). User on loaded CPU may hear glitches.
fn promote_rt_once(flag: &AtomicBool, sample_rate: u32) {
    if !flag.swap(true, Ordering::AcqRel) {
        // First time: try promote.
        match audio_thread_priority::promote_current_thread_to_real_time(
            /* audio_buffer_frames */ 256,
            sample_rate,
        ) {
            Ok(_handle) => {
                tracing::info!(sample_rate, "audio thread promoted to RT priority");
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "audio thread RT promotion failed (continuing with default priority — glitches possible under CPU load)"
                );
            }
        }
    }
}

/// Push as many samples as fit in the ring buffer; increment overflow counter
/// for samples dropped.
///
/// Per code-reviewer C2 fix: `rtrb::push_entire_slice` is all-or-nothing.
/// Under transient consumer slow-down, dropping the WHOLE 1024-sample block
/// (~21ms) is worse than dropping just the trailing samples that don't fit.
/// We compute available slots first and push only that many.
#[inline]
fn push_partial(tx: &mut rtrb::Producer<i16>, samples: &[i16], overflow: &OverflowCounter) {
    let available = tx.slots();
    let to_push = available.min(samples.len());
    if to_push > 0 {
        let _ = tx.push_entire_slice(&samples[..to_push]);
    }
    let dropped = samples.len().saturating_sub(to_push);
    if dropped > 0 {
        overflow.add(dropped as u64);
    }
}

/// Forward f32 samples → i16, downmix on the fly if channels > 1 → still
/// preserve as interleaved (downmix to mono happens later in resampler).
///
/// Stack-only: `tmp` buffer is sized at `MAX_CALLBACK_BLOCK` and bounded.
#[inline]
fn forward_f32(
    data: &[f32],
    channels: u16,
    tx: &mut rtrb::Producer<i16>,
    peak: &PeakMeter,
    overflow: &OverflowCounter,
) {
    let mut tmp = [0i16; MAX_CALLBACK_BLOCK];
    let n = data.len().min(MAX_CALLBACK_BLOCK);
    let mut local_peak = 0f32;

    for i in 0..n {
        let s = data[i];
        let abs = s.abs();
        if abs > local_peak {
            local_peak = abs;
        }
        // Quantize f32 in [-1.0, 1.0] → i16
        let q = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        tmp[i] = q;
    }

    push_partial(tx, &tmp[..n], overflow);
    peak.update(local_peak);

    let _ = channels; // currently informational — downmix handled in resampler
}

#[inline]
fn forward_i16(
    data: &[i16],
    channels: u16,
    tx: &mut rtrb::Producer<i16>,
    peak: &PeakMeter,
    overflow: &OverflowCounter,
) {
    let n = data.len().min(MAX_CALLBACK_BLOCK);
    let mut local_peak = 0i32;

    for &s in &data[..n] {
        let abs = (s as i32).abs();
        if abs > local_peak {
            local_peak = abs;
        }
    }

    push_partial(tx, &data[..n], overflow);
    peak.update(local_peak as f32 / i16::MAX as f32);
    let _ = channels;
}

#[inline]
fn forward_u16(
    data: &[u16],
    channels: u16,
    tx: &mut rtrb::Producer<i16>,
    peak: &PeakMeter,
    overflow: &OverflowCounter,
) {
    let mut tmp = [0i16; MAX_CALLBACK_BLOCK];
    let n = data.len().min(MAX_CALLBACK_BLOCK);
    let mut local_peak = 0i32;

    for i in 0..n {
        // u16 [0, 65535] center 32768 → i16 [-32768, 32767]
        let s = (data[i] as i32 - 32768) as i16;
        tmp[i] = s;
        let abs = (s as i32).abs();
        if abs > local_peak {
            local_peak = abs;
        }
    }

    push_partial(tx, &tmp[..n], overflow);
    peak.update(local_peak as f32 / i16::MAX as f32);
    let _ = channels;
}

#[inline]
fn forward_u8(
    data: &[u8],
    channels: u16,
    tx: &mut rtrb::Producer<i16>,
    peak: &PeakMeter,
    overflow: &OverflowCounter,
) {
    let mut tmp = [0i16; MAX_CALLBACK_BLOCK];
    let n = data.len().min(MAX_CALLBACK_BLOCK);
    let mut local_peak = 0i32;

    for i in 0..n {
        // u8 [0, 255] center 128 → i16 [-32768, 32512]. Left-shift 8 scales 8-bit
        // dynamic range up to 16-bit container; LSB stays zero (no fake precision).
        let s = ((data[i] as i32 - 128) << 8) as i16;
        tmp[i] = s;
        let abs = (s as i32).abs();
        if abs > local_peak {
            local_peak = abs;
        }
    }

    push_partial(tx, &tmp[..n], overflow);
    peak.update(local_peak as f32 / i16::MAX as f32);
    let _ = channels;
}
