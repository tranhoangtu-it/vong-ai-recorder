//! Vọng UI subsystem.
//!
//! Phase 5: System Tray (3 dynamic icons) + Floating Pill (acrylic vibrancy)
//!          + Dashboard window stub + global hotkey Cmd+Shift+R.
//! Phase 6: Dashboard History tab full impl.
//! Phase 7: Onboarding wizard (60-second flow) + permission probing.

#![warn(missing_docs)]
#![warn(clippy::all)]

/// Phase 0 placeholder. Phase 5+ exports tray, pill, dashboard modules.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
