//! Audio input + output device enumeration + stream config negotiation.
//!
//! Phase 1: input mic capture. Phase 2-W: output loopback capture (system audio).
//! On Windows + WASAPI, calling `build_input_stream` on an *output* device
//! automatically opens the stream in `AUDCLNT_STREAMFLAGS_LOOPBACK` mode —
//! cpal 0.17 handles this transparently.
//!
//! Plan v2 Section 13.1.4 — sample rate fallback ladder 48k → 44.1k → device default.

use crate::error::AudioError;
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{Device, StreamConfig, SupportedStreamConfig};

/// Whether a `DeviceDescriptor` represents an input (mic / line-in) or output
/// (speakers / headphones — captured via WASAPI loopback on Windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// Microphone or other capture device.
    Input,
    /// Speaker / output device — captured as loopback (Windows-specific WASAPI feature).
    Output,
}

impl DeviceKind {
    /// Stable token used in config files + UI labels.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "loopback",
        }
    }
}

impl std::str::FromStr for DeviceKind {
    type Err = ();

    /// Parse the token. Accepts case-insensitive `"input"|"mic"|"microphone"`
    /// or `"output"|"loopback"|"speaker"|"speakers"`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "input" | "mic" | "microphone" => Ok(Self::Input),
            "output" | "loopback" | "speaker" | "speakers" => Ok(Self::Output),
            _ => Err(()),
        }
    }
}

/// Lightweight device descriptor for UI dropdown / preference persistence.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Device display name (e.g., "MacBook Pro Microphone").
    pub name: String,
    /// Input vs output (loopback).
    pub kind: DeviceKind,
    /// Whether this is the host's default device for its kind.
    pub is_default: bool,
}

/// Enumerate all input devices on the default host.
///
/// NOTE: `cpal::DeviceTrait::name()` is deprecated in 0.17 in favor of
/// `description()` + `id()`. We use `name()` for now since it returns
/// a simple `String` suitable for UI dropdown. Phase 5+ may switch to
/// `description()` + stable `id()` for device-persistence across reboots.
#[allow(deprecated)]
pub fn list_input_devices() -> Result<Vec<DeviceInfo>, AudioError> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();

    let devices = host
        .input_devices()?
        .filter_map(|d| {
            let name = d.name().ok()?;
            Some(DeviceInfo {
                is_default: name == default_name,
                kind: DeviceKind::Input,
                name,
            })
        })
        .collect();

    Ok(devices)
}

/// Enumerate every input + output device. On Windows the outputs can be
/// captured as loopback (system audio) — see module docs.
#[allow(deprecated)]
pub fn list_all_devices() -> Result<Vec<DeviceInfo>, AudioError> {
    let host = cpal::default_host();
    let default_in = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();
    let default_out = host
        .default_output_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();

    let mut out = Vec::new();

    if let Ok(inputs) = host.input_devices() {
        for d in inputs {
            if let Ok(name) = d.name() {
                let is_default = name == default_in;
                out.push(DeviceInfo {
                    is_default,
                    kind: DeviceKind::Input,
                    name,
                });
            }
        }
    }

    if let Ok(outputs) = host.output_devices() {
        for d in outputs {
            if let Ok(name) = d.name() {
                let is_default = name == default_out;
                out.push(DeviceInfo {
                    is_default,
                    kind: DeviceKind::Output,
                    name,
                });
            }
        }
    }

    Ok(out)
}

/// Return the host's default input device, if any.
pub fn default_input_device() -> Result<Device, AudioError> {
    cpal::default_host()
        .default_input_device()
        .ok_or(AudioError::NoInputDevice)
}

/// Look up a specific device by name + kind. Used by the audio-source picker
/// when the user has saved a preferred capture source.
#[allow(deprecated)]
pub fn find_device(name: &str, kind: DeviceKind) -> Result<Device, AudioError> {
    let host = cpal::default_host();
    let iter = match kind {
        DeviceKind::Input => host
            .input_devices()
            .map_err(|e| AudioError::DeviceEnumeration(e.to_string()))?,
        DeviceKind::Output => host
            .output_devices()
            .map_err(|e| AudioError::DeviceEnumeration(e.to_string()))?,
    };
    for d in iter {
        if let Ok(n) = d.name() {
            if n == name {
                return Ok(d);
            }
        }
    }
    Err(AudioError::NoInputDevice)
}

/// Negotiate a stream configuration with the device.
///
/// Tries preferred rates in order, falls back to device default if none match.
/// Plan v2 Section 13.1.4 — fallback ladder 48k → 44.1k → device default.
///
/// `kind` controls which config family is queried:
/// - `Input` — `supported_input_configs` + `default_input_config` (mic / line-in)
/// - `Output` — `supported_output_configs` + `default_output_config`; cpal's
///   WASAPI backend then opens the matching `build_input_stream` call in
///   `AUDCLNT_STREAMFLAGS_LOOPBACK` mode automatically (Windows-specific).
pub fn negotiate_config(
    device: &Device,
    kind: DeviceKind,
) -> Result<SupportedStreamConfig, AudioError> {
    let preferred_rates = [48_000u32, 44_100];

    let try_match = |configs: Box<dyn Iterator<Item = cpal::SupportedStreamConfigRange>>| -> Option<SupportedStreamConfig> {
        for cfg_range in configs {
            for &rate in &preferred_rates {
                let min = cfg_range.min_sample_rate();
                let max = cfg_range.max_sample_rate();
                if min <= rate && rate <= max {
                    return Some(cfg_range.with_sample_rate(rate));
                }
            }
        }
        None
    };

    let matched = match kind {
        DeviceKind::Input => device
            .supported_input_configs()
            .ok()
            .and_then(|cfgs| try_match(Box::new(cfgs))),
        DeviceKind::Output => device
            .supported_output_configs()
            .ok()
            .and_then(|cfgs| try_match(Box::new(cfgs))),
    };
    if let Some(cfg) = matched {
        return Ok(cfg);
    }

    // Fallback: device default for the requested kind.
    let default = match kind {
        DeviceKind::Input => device.default_input_config()?,
        DeviceKind::Output => device.default_output_config()?,
    };
    let rate_hz = default.sample_rate();
    tracing::warn!(
        rate_hz,
        kind = kind.as_str(),
        "negotiate_config falling back to device default (preferred 48k/44.1k unsupported)"
    );
    Ok(default)
}

/// Convert `SupportedStreamConfig` → `StreamConfig` for `build_input_stream`.
pub fn to_stream_config(supported: &SupportedStreamConfig) -> StreamConfig {
    let mut cfg: StreamConfig = supported.config();
    // Suggest a reasonable buffer size. Some devices accept arbitrary, others fixed.
    cfg.buffer_size = cpal::BufferSize::Default;
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_devices_does_not_panic() {
        // May return empty list on CI without audio hardware, but should not panic.
        let _ = list_input_devices();
    }

    #[test]
    fn default_device_either_succeeds_or_returns_no_device() {
        // Either OK with a device, or NoInputDevice. Anything else is wrong.
        match default_input_device() {
            Ok(_) | Err(AudioError::NoInputDevice) => {}
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
}
