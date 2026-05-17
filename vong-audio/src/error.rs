//! Audio subsystem error types.

use thiserror::Error;

/// Errors emitted by the audio capture/resample/mix pipeline.
#[derive(Debug, Error)]
pub enum AudioError {
    /// No input audio device found on the host.
    #[error("no input audio device available on host")]
    NoInputDevice,

    /// Device enumeration failed.
    #[error("device enumeration failed: {0}")]
    DeviceEnumeration(String),

    /// Default device config could not be queried.
    #[error("default config query failed: {0}")]
    DefaultConfig(String),

    /// Requested stream configuration not supported by device.
    #[error("stream configuration unsupported: requested {requested_hz}Hz on device '{device}'")]
    UnsupportedConfig {
        /// Device name (sanitized for logs).
        device: String,
        /// Requested sample rate in Hz.
        requested_hz: u32,
    },

    /// cpal-level stream build error.
    #[error("stream build failed: {0}")]
    StreamBuild(String),

    /// cpal-level stream play error.
    #[error("stream play failed: {0}")]
    StreamPlay(String),

    /// Resampler initialization failure.
    #[error("resampler init failed: {0}")]
    ResamplerInit(String),

    /// Resampler runtime failure (e.g. ratio out of bounds).
    #[error("resampler runtime error: {0}")]
    Resampler(String),

    /// Failed to promote thread to real-time priority.
    /// Non-fatal — capture continues with default priority but glitches possible under load.
    #[error("RT thread promotion failed: {0}")]
    RtPromotion(String),

    /// Downstream consumer dropped (audio output channel closed).
    #[error("downstream consumer dropped")]
    ConsumerDropped,
}

impl From<cpal::DevicesError> for AudioError {
    fn from(e: cpal::DevicesError) -> Self {
        Self::DeviceEnumeration(e.to_string())
    }
}

impl From<cpal::DefaultStreamConfigError> for AudioError {
    fn from(e: cpal::DefaultStreamConfigError) -> Self {
        Self::DefaultConfig(e.to_string())
    }
}

impl From<cpal::BuildStreamError> for AudioError {
    fn from(e: cpal::BuildStreamError) -> Self {
        Self::StreamBuild(e.to_string())
    }
}

impl From<cpal::PlayStreamError> for AudioError {
    fn from(e: cpal::PlayStreamError) -> Self {
        Self::StreamPlay(e.to_string())
    }
}

impl From<rubato::ResamplerConstructionError> for AudioError {
    fn from(e: rubato::ResamplerConstructionError) -> Self {
        Self::ResamplerInit(e.to_string())
    }
}

impl From<rubato::ResampleError> for AudioError {
    fn from(e: rubato::ResampleError) -> Self {
        Self::Resampler(e.to_string())
    }
}
