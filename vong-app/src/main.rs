//! Vọng STT — Main entry point.
//!
//! Phase 0: Minimal Slint Hello window to verify workspace scaffold.
//! Phase 1+: Wire audio capture, STT, UI subsystems.

mod log_init;

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    log_init::init_logging();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "Vọng starting"
    );

    let ui = AppWindow::new()?;
    ui.run()?;

    tracing::info!("Vọng shutting down");
    Ok(())
}
