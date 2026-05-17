//! Vọng audio subsystem.
//!
//! Phase 1: Mic capture (cpal + RT thread + ring buffer + resampler)
//! Phase 2: System audio loopback + adaptive resampler mix (clock drift)
//! Phase 3: VAD + state machine (utterance packaging)
//!
//! # Quick start (Phase 1 pipeline)
//!
//! ```no_run
//! use vong_audio::{default_input_device, start_capture, run_resampler,
//!     OverflowCounter, PeakMeter, ResampleConfig};
//! use rtrb::RingBuffer;
//! use tokio::sync::mpsc;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Ring buffer: ~5 seconds at 48kHz stereo = 480_000 samples.
//! let (tx, rx) = RingBuffer::<i16>::new(480_000);
//!
//! let device = default_input_device()?;
//! let peak = PeakMeter::new();
//! let overflow = OverflowCounter::new();
//! let handle = start_capture(&device, tx, peak.clone(), overflow.clone())?;
//!
//! // Resampler emits 16k mono PCM16 chunks via mpsc.
//! let (out_tx, mut out_rx) = mpsc::channel(64);
//! let cfg = ResampleConfig {
//!     source_rate: handle.config().sample_rate,
//!     source_channels: handle.config().channels,
//!     ..Default::default()
//! };
//! tokio::spawn(async move {
//!     let _ = run_resampler(rx, out_tx, cfg).await;
//! });
//!
//! while let Some(chunk_16k) = out_rx.recv().await {
//!     // Forward to VAD (Phase 3) or Soniox (Phase 4)
//!     println!("got {} samples @ 16kHz", chunk_16k.len());
//! }
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]

mod capture;
mod device;
mod error;
mod resample;
mod types;
mod utterance;
mod vad;

pub use capture::{start_capture, CaptureHandle};
pub use device::{default_input_device, list_input_devices, negotiate_config, DeviceInfo};
pub use error::AudioError;
pub use resample::{run_resampler, ResampleConfig};
pub use types::{
    AudioConfig, AudioSource, OverflowCounter, PeakMeter, MAX_CALLBACK_BLOCK, TARGET_CHANNELS,
    TARGET_SAMPLE_RATE_HZ,
};
pub use utterance::{PreRollBuffer, Utterance, UtteranceBuilder};
pub use vad::{run_vad_fsm, VadConfig, VadFsm, VAD_FRAME_MS, VAD_FRAME_SAMPLES};

/// Vọng audio subsystem version.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
