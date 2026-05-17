//! Audio input device enumeration + stream config negotiation.
//!
//! Plan v2 Section 13.1.4 — sample rate fallback ladder 48k → 44.1k → device default.

use crate::error::AudioError;
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{Device, StreamConfig, SupportedStreamConfig};

/// Lightweight device descriptor for UI dropdown.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Device display name (e.g., "MacBook Pro Microphone").
    pub name: String,
    /// Whether this is the host's default input device.
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
                name,
            })
        })
        .collect();

    Ok(devices)
}

/// Return the host's default input device, if any.
pub fn default_input_device() -> Result<Device, AudioError> {
    cpal::default_host()
        .default_input_device()
        .ok_or(AudioError::NoInputDevice)
}

/// Negotiate a stream configuration with the device.
///
/// Tries preferred rates in order, falls back to device default if none match.
/// Plan v2 Section 13.1.4 — fallback ladder 48k → 44.1k → device default.
pub fn negotiate_config(device: &Device) -> Result<SupportedStreamConfig, AudioError> {
    let preferred_rates = [48_000u32, 44_100];

    if let Ok(configs) = device.supported_input_configs() {
        for cfg_range in configs {
            for &rate in &preferred_rates {
                let min = cfg_range.min_sample_rate();
                let max = cfg_range.max_sample_rate();
                if min <= rate && rate <= max {
                    return Ok(cfg_range.with_sample_rate(rate));
                }
            }
        }
    }

    // Fallback: device default
    let default = device.default_input_config()?;
    let rate_hz = default.sample_rate();
    tracing::warn!(
        rate_hz,
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
