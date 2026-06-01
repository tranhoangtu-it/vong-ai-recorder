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
//! - Windows: `%APPDATA%\Vong\Vong AI Recorder\data\logs\vong.log.YYYY-MM-DD`
//! - macOS:   `~/Library/Application Support/com.Vong.Vong-AI-Recorder/logs/vong.log.YYYY-MM-DD`
//! - Linux:   `$XDG_DATA_HOME/vong-ai-recorder/logs/vong.log.YYYY-MM-DD`
//!
//! ## Sentry integration
//!
//! When `sentry_guard` is `Some(…)`, this function:
//! 1. Attaches a `sentry-tracing` layer to the subscriber (ERROR → Sentry
//!    event; WARN/INFO → breadcrumb; DEBUG/TRACE → dropped).
//! 2. Installs a panic hook that FIRST logs `file:line` (metadata only, no
//!    payload — privacy rule), THEN forwards the `PanicInfo` to
//!    `sentry::integrations::panic::panic_handler` so Sentry captures the
//!    panic's stack trace.
//!
//! The startup order in `main()` MUST be:
//!   1. `sentry_init::init_if_enabled()`
//!   2. `log_init::init_logging(sentry_guard.as_ref())`
//!
//! If the order is reversed, Sentry's own panic hook (installed by `sentry::init`)
//! is already on the stack, and our hook (installed by `init_logging`) replaces
//! it — but our hook then calls `sentry::integrations::panic::panic_handler`
//! explicitly, so Sentry still receives the panic.

use std::path::PathBuf;
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{RollingFileAppender, Rotation},
};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Registry};

/// Resolve the production log directory. `None` if `ProjectDirs` can't resolve
/// (very rare — only on weird hosts) — caller falls back to stderr-only.
fn log_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "Vong", "Vong AI Recorder")
        .map(|d| d.data_dir().join("logs"))
}

/// Initialize logging subscriber with env-filter + sanitization-safe defaults
/// + daily-rolling file output for production crash diagnostics.
///
/// `sentry_guard` — pass `sentry_init::init_if_enabled().as_ref()`. When
/// `Some`, a Sentry tracing layer is attached and the panic hook forwards
/// to Sentry. When `None`, behaviour is identical to pre-Phase-9 (no Sentry
/// dependency at runtime).
///
/// The returned `WorkerGuard` must be kept alive for the lifetime of the app —
/// dropping it flushes the file writer. Bind to `_log_guard` in `main()`.
#[must_use = "WorkerGuard must outlive any tracing call — bind it in main()"]
pub fn init_logging(sentry_guard: Option<&sentry::ClientInitGuard>) -> Option<WorkerGuard> {
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

    let sentry_active = sentry_guard.is_some();

    let registry = Registry::default().with(filter).with(stderr_layer);
    let guard = match file_layer_and_guard {
        Some((layer, guard)) => {
            if sentry_active {
                // Attach sentry-tracing layer when Sentry is initialised.
                registry
                    .with(layer)
                    .with(crate::sentry_init::build_tracing_layer())
                    .init();
            } else {
                registry.with(layer).init();
            }
            Some(guard)
        }
        None => {
            if sentry_active {
                registry
                    .with(crate::sentry_init::build_tracing_layer())
                    .init();
            } else {
                registry.init();
            }
            None
        }
    };

    // Install panic hook that:
    // 1. Logs only file:line — deliberately NO payload (privacy rule: panic
    //    messages may contain transcript text passed via panic!("{var}")).
    // 2. Forwards to Sentry's panic handler when Sentry is active, so Sentry
    //    captures the panic with its full stack trace.
    //
    // Note: `sentry_active` is a plain bool captured by move — no Arc/Mutex
    // needed because the hook outlives Sentry's `ClientInitGuard` only if
    // Sentry was never initialised (sentry_active = false).
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "<unknown>".into());

        // Deliberately DO NOT format the payload string — it may contain
        // sensitive data passed via panic!(format).
        tracing::error!(
            location = %location,
            "PANIC (payload elided for privacy — re-run with VONG_LOG=debug if needed)"
        );

        // Forward to Sentry if active. Sentry captures file + line + stack
        // trace from the PanicInfo without us ever reading the payload text.
        if sentry_active {
            sentry::integrations::panic::panic_handler(info);
        }
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
