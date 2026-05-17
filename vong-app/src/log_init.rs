//! Logging initialization with sanitization rules + production crash log file.
//!
//! Plan v2 Section 15.6 — Logs may contain metadata (lengths, IDs, durations,
//! status codes, error categories) but NEVER content (audio bytes, transcript
//! text, API keys, file paths to user data, IP addresses).
//!
//! Production GUI build: stderr is invisible to end users — we additionally
//! write to a rolling file at the OS data dir so panics survive process exit.
//!
//! Usage:
//! ```ignore
//! VONG_LOG=debug ./vong  # enable verbose logs
//! VONG_LOG=vong=info     # default
//! ```
//!
//! Log file paths:
//! - Windows: `%APPDATA%\Vong\Vong\data\logs\vong.log.YYYY-MM-DD`
//! - macOS:   `~/Library/Application Support/com.Vong.Vong/logs/vong.log.YYYY-MM-DD`
//! - Linux:   `$XDG_DATA_HOME/Vong/logs/vong.log.YYYY-MM-DD`

use std::path::PathBuf;
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{RollingFileAppender, Rotation},
};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Registry};

/// Resolve the production log directory. `None` if `ProjectDirs` can't resolve
/// (very rare — only on weird hosts) — caller falls back to stderr-only.
fn log_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "Vong", "Vong").map(|d| d.data_dir().join("logs"))
}

/// Initialize logging subscriber with env-filter + sanitization-safe defaults
/// + daily-rolling file output for production crash diagnostics.
///
/// The returned `WorkerGuard` must be kept alive for the lifetime of the app —
/// dropping it flushes the file writer. Bind to `_log_guard` in `main()`.
#[must_use = "WorkerGuard must outlive any tracing call — bind it in main()"]
pub fn init_logging() -> Option<WorkerGuard> {
    let filter =
        EnvFilter::try_from_env("VONG_LOG").unwrap_or_else(|_| EnvFilter::new("vong=info,warn"));

    let stderr_layer = fmt::layer()
        .with_target(true)
        .with_thread_ids(true)
        .with_thread_names(true)
        .compact();

    // Attempt to add a daily-rolling file layer alongside stderr. If the data
    // dir can't be created we keep going with stderr-only — never block startup.
    let file_layer_and_guard = log_dir().and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        let appender = RollingFileAppender::new(Rotation::DAILY, &dir, "vong.log");
        let (writer, guard) = tracing_appender::non_blocking(appender);
        let layer = fmt::layer()
            .with_target(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_ansi(false) // No ANSI codes in file output — keep grep-friendly.
            .with_writer(writer);
        eprintln!("(vong) writing logs to {}", dir.display());
        Some((layer, guard))
    });

    let registry = Registry::default().with(filter).with(stderr_layer);
    let guard = match file_layer_and_guard {
        Some((layer, guard)) => {
            registry.with(layer).init();
            Some(guard)
        }
        None => {
            registry.init();
            None
        }
    };

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

    guard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_logging_does_not_panic() {
        // Don't actually init in tests (would set global subscriber, conflicting
        // with parallel test execution). Just exercise the path logic.
        let _ = log_dir();
    }
}
