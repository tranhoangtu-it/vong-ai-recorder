//! Vọng audio subsystem.
//!
//! Phase 1: Mic capture (cpal + RT thread + ring buffer)
//! Phase 2: System audio loopback + adaptive resampler mix (clock drift)
//! Phase 3: VAD + state machine (utterance packaging)
//!
//! Public API stable from Phase 1 forward.

#![warn(missing_docs)]
#![warn(clippy::all)]

/// Phase 0 placeholder. Will export public types Phase 1+.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_not_empty() {
        assert!(!super::version().is_empty());
    }
}
