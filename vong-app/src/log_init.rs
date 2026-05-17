//! Logging initialization với sanitization rules.
//!
//! Plan v2 Section 15.6 — Logs may contain metadata (lengths, IDs, durations,
//! status codes, error categories) but NEVER content (audio bytes, transcript
//! text, API keys, file paths to user data, IP addresses).
//!
//! Usage:
//! ```ignore
//! VONG_LOG=debug ./vong  # enable verbose logs
//! VONG_LOG=vong=info     # default
//! ```

use tracing_subscriber::{fmt, EnvFilter};

/// Initialize logging subscriber with env-filter + sanitization-safe defaults.
pub fn init_logging() {
    let filter = EnvFilter::try_from_env("VONG_LOG")
        .unwrap_or_else(|_| EnvFilter::new("vong=info,warn"));

    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(true)
        .with_thread_names(true)
        .compact()
        .init();

    // Install panic hook that sanitizes — no API keys, no audio content,
    // no user file paths leak via panic message.
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "<unknown>".into());

        // Note: We deliberately DO NOT format the payload string here —
        // it might contain sensitive data passed via panic!(format).
        tracing::error!(
            location = %location,
            "PANIC (payload elided for privacy — re-run with VONG_LOG=debug if needed)"
        );
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_logging_does_not_panic() {
        init_logging();
    }
}
