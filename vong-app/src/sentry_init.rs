//! Sentry crash reporting — opt-in only, privacy-scrubbed.
//!
//! Default OFF. User explicitly enables via the first-run wizard step or the
//! Settings → System toggle. Consent is persisted to:
//!   `%APPDATA%\Vong\Vong AI Recorder\config\sentry.txt`
//!
//! ## Pre-merge user action (REQUIRED before tagging a release)
//!
//! 1. Create a Sentry account at https://sentry.io (free tier).
//! 2. Create a project named "vong-ai-recorder" with platform "Rust".
//! 3. Enable "Prevent Storing of IP Addresses" in Project Settings →
//!    Security & Privacy.
//! 4. Copy the DSN from Project Settings → Client Keys (DSN).
//! 5. Replace the `REPLACE-ME-WITH-VONG-OWNED-DSN` placeholder in `SENTRY_DSN`
//!    below with the real DSN.
//!
//! ## Privacy model
//!
//! Four-layer defence (belt + suspenders + belt + suspenders):
//! 1. CLAUDE.md "metadata only, NEVER content" code-time rule.
//! 2. `sentry-tracing` `event_filter`: DEBUG + TRACE dropped entirely.
//!    INFO + WARN → breadcrumbs; ERROR → full Sentry event.
//! 3. `before_send` scrubs server_name, user, extra, oversized payloads.
//! 4. Sentry server-side: "Prevent Storing of IP Addresses" (project setting).

use std::io::{self, Write};
use std::path::PathBuf;

// ── DSN constant (REPLACE before release) ────────────────────────────────────
//
// IMPORTANT: Replace this placeholder with the real Vong-owned DSN before
// merging to a release branch. The DSN is NOT a secret — it is safe to embed
// in a compiled binary. See the module-level doc for setup instructions.
//
// The init_if_enabled() function detects this placeholder string and returns
// None (Sentry stays disabled), so shipping with the placeholder is safe —
// just not useful.
pub const SENTRY_DSN: &str =
    "REPLACE-ME-WITH-VONG-OWNED-DSN-https://public@oXXXX.ingest.sentry.io/XXXX";

// ── Consent state ─────────────────────────────────────────────────────────────

/// Whether the user has opted in, opted out, or has not yet been asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentState {
    /// User explicitly enabled crash reporting.
    Enabled,
    /// User explicitly disabled crash reporting.
    Disabled,
    /// No consent file present — treat as disabled (never silently enable).
    Absent,
}

// ── File path helpers ─────────────────────────────────────────────────────────

/// Path to the crash-reporting consent file.
/// `%APPDATA%\Vong\Vong AI Recorder\config\sentry.txt`
pub fn consent_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "Vong", "Vong AI Recorder")
        .map(|d| d.config_dir().join("sentry.txt"))
}

// ── Consent persistence ───────────────────────────────────────────────────────

/// Read the consent state from disk.
///
/// - Missing file → `Absent` (treated as disabled).
/// - File content = `"enabled"` → `Enabled`.
/// - File content = `"disabled"` → `Disabled`.
/// - Anything else → `Absent`.
pub fn load_consent() -> ConsentState {
    let Some(path) = consent_path() else {
        return ConsentState::Absent;
    };
    load_consent_from_path(&path)
}

/// Inner helper — reads consent from an arbitrary path.
/// Extracted so unit tests can inject a tempdir path without touching AppData.
pub(crate) fn load_consent_from_path(path: &std::path::Path) -> ConsentState {
    let Ok(content) = std::fs::read_to_string(path) else {
        return ConsentState::Absent;
    };
    match content.trim() {
        "enabled" => ConsentState::Enabled,
        "disabled" => ConsentState::Disabled,
        _ => ConsentState::Absent,
    }
}

/// Persist the consent state to disk.
///
/// Writing `Absent` is a no-op (no file created or modified).
pub fn save_consent(state: ConsentState) -> io::Result<()> {
    let path = consent_path()
        .ok_or_else(|| io::Error::other("no config dir — cannot save sentry consent"))?;
    save_consent_to_path(&path, state)
}

/// Inner helper — writes consent to an arbitrary path.
/// Extracted so unit tests can inject a tempdir path without touching AppData.
pub(crate) fn save_consent_to_path(path: &std::path::Path, state: ConsentState) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let value = match state {
        ConsentState::Enabled => "enabled\n",
        ConsentState::Disabled => "disabled\n",
        ConsentState::Absent => return Ok(()), // no-op — do not create the file
    };
    // write + create + truncate to overwrite any prior value atomically.
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    f.write_all(value.as_bytes())
}

// ── Sentry initialisation ─────────────────────────────────────────────────────

/// Initialise Sentry if and only if the user has opted in AND the DSN is a
/// real value (not the placeholder).
///
/// Returns `Some(ClientInitGuard)` when Sentry is started; the caller MUST
/// bind this to a top-level variable so the guard (which flushes pending
/// events on drop) outlives the entire process.
///
/// Returns `None` when:
/// - Consent file says `disabled` or is absent (default OFF).
/// - `SENTRY_DSN` still contains the placeholder string "REPLACE-ME".
/// - The DSN string fails to parse.
///
/// Call this BEFORE `log_init::init_logging()` — our panic hook, installed
/// by `log_init`, explicitly calls `sentry::integrations::panic::panic_handler`
/// to forward panics to Sentry. If log_init runs first its hook is on top but
/// references our closure that calls the Sentry handler.
pub fn init_if_enabled() -> Option<sentry::ClientInitGuard> {
    // 1. Consent gate — never silently enable.
    if !matches!(load_consent(), ConsentState::Enabled) {
        return None;
    }

    // 2. DSN placeholder guard — shipping without a real DSN is safe but inert.
    if SENTRY_DSN.contains("REPLACE-ME") {
        // Note: Do NOT log the DSN value — even the placeholder should not
        // appear in log files (production DSN will replace it and is then
        // embedded in the binary; no need to echo it to disk).
        return None;
    }

    // 3. Parse DSN.
    let dsn: sentry::types::Dsn = match SENTRY_DSN.parse() {
        Ok(d) => d,
        Err(_) => {
            // Intentionally no detail in the message — the DSN parse error
            // string may repeat the DSN value.
            eprintln!("(vong) Sentry DSN parse failed — crash reporting disabled");
            return None;
        }
    };

    // 4. Build Sentry environment tag.
    let environment = if cfg!(debug_assertions) {
        "development"
    } else {
        "beta"
    };

    let guard = sentry::init(sentry::ClientOptions {
        dsn: Some(dsn),
        release: Some(
            format!("vong-ai-recorder@{}", env!("CARGO_PKG_VERSION")).into(),
        ),
        environment: Some(environment.into()),
        sample_rate: 1.0,
        traces_sample_rate: 0.0,   // no performance monitoring
        send_default_pii: false,   // no IP / username capture
        before_send: Some(std::sync::Arc::new(before_send)),
        ..Default::default()
    });

    Some(guard)
}

// ── before_send scrubber ──────────────────────────────────────────────────────

/// Scrub every outgoing event before it reaches Sentry's servers.
///
/// Privacy invariants enforced here:
/// - `server_name`: cleared (Windows hostname leaks machine identity).
/// - `user`: cleared (we never set it, but be defensive).
/// - `extra`: cleared (we never set it, but be defensive).
/// - `tags`: only `environment` and `release` are kept (we set those
///   deliberately; everything else is OS auto-collected and may carry paths).
/// - `message` > 200 chars: replaced with `"[redacted payload]"`. Panic
///   payloads sometimes carry user data via `panic!("{var}")`.
/// - `exception.value` > 200 chars: same redaction.
fn before_send(mut event: sentry::protocol::Event<'static>) -> Option<sentry::protocol::Event<'static>> {
    // 1. Drop Windows hostname — typically "DESKTOP-XYZ123" or user's machine name.
    event.server_name = None;

    // 2. Drop user context — we never set it, but block any SDK-side collection.
    event.user = None;

    // 3. Drop arbitrary extra map.
    event.extra.clear();

    // 4. Retain only the two tags we set intentionally; drop all SDK auto-tags.
    event
        .tags
        .retain(|k, _| matches!(k.as_str(), "environment" | "release"));

    // 5. Redact oversized message bodies. A panic message of > 200 chars is
    //    almost certainly carrying user data (e.g. transcript text in format!()).
    if let Some(ref msg) = event.message {
        if msg.len() > 200 {
            event.message = Some("[redacted payload]".to_string());
        }
    }

    // 6. Same redaction on exception values (the panic string in the exception
    //    entry may differ from event.message for unwrap/expect panics).
    for exc in &mut event.exception.values {
        if let Some(ref val) = exc.value {
            if val.len() > 200 {
                exc.value = Some("[redacted payload]".to_string());
            }
        }
    }

    Some(event)
}

// ── sentry-tracing layer builder ──────────────────────────────────────────────

/// Build the `sentry-tracing` subscriber layer.
///
/// Returns a type-erased `Box<dyn tracing_subscriber::Layer<S> + Send + Sync>`
/// so the caller does not need to name the concrete `SentryLayer<…>` type,
/// which changes with every `.with(…)` call in the subscriber chain.
///
/// Filter rules (belt-and-suspenders on top of CLAUDE.md privacy rule):
/// - `ERROR` → full Sentry event (stack trace + breadcrumbs).
/// - `WARN` / `INFO` → breadcrumb (context for the preceding error).
/// - `DEBUG` / `TRACE` → **dropped** entirely. These levels may carry
///   `transcript_len`, `audio_bytes`, dictionary counts, etc. Safe metadata
///   by themselves, but we draw the line conservatively.
pub fn build_tracing_layer<S>() -> Box<dyn tracing_subscriber::Layer<S> + Send + Sync>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use sentry_tracing::EventFilter;
    Box::new(
        sentry_tracing::layer().event_filter(|metadata| match *metadata.level() {
            tracing::Level::ERROR => EventFilter::Event,
            tracing::Level::WARN | tracing::Level::INFO => EventFilter::Breadcrumb,
            _ => EventFilter::Ignore,
        }),
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sentry::protocol::Event;
    use tempfile::TempDir;

    // ── before_send scrubbing tests ──────────────────────────────────────────

    #[test]
    fn before_send_strips_server_name() {
        let e = Event {
            server_name: Some("DESKTOP-ABC123".into()),
            ..Default::default()
        };
        let scrubbed = before_send(e).expect("event must be Some after scrub");
        assert!(scrubbed.server_name.is_none(), "server_name must be cleared");
    }

    #[test]
    fn before_send_strips_user() {
        let e = Event {
            user: Some(sentry::User {
                username: Some("test-user".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let scrubbed = before_send(e).expect("event must be Some after scrub");
        assert!(scrubbed.user.is_none(), "user must be cleared");
    }

    #[test]
    fn before_send_clears_extra() {
        let mut e = Event::<'static>::default();
        // extra is a BTreeMap — insert after construction (no field-init syntax for maps)
        e.extra.insert("some_key".to_string(), serde_json::json!("some_value"));
        let scrubbed = before_send(e).expect("event must be Some after scrub");
        assert!(scrubbed.extra.is_empty(), "extra must be cleared");
    }

    #[test]
    fn before_send_redacts_oversized_message() {
        let e = Event {
            message: Some("x".repeat(300)),
            ..Default::default()
        };
        let scrubbed = before_send(e).expect("event must be Some after scrub");
        assert_eq!(
            scrubbed.message.as_deref(),
            Some("[redacted payload]"),
            "message > 200 chars must be redacted"
        );
    }

    #[test]
    fn before_send_preserves_normal_message() {
        let e = Event {
            message: Some("Connection to Soniox WS failed".into()),
            ..Default::default()
        };
        let scrubbed = before_send(e).expect("event must be Some after scrub");
        assert_eq!(
            scrubbed.message.as_deref(),
            Some("Connection to Soniox WS failed"),
            "short message must pass through unchanged"
        );
    }

    #[test]
    fn before_send_redacts_oversized_exception_value() {
        let exc = sentry::protocol::Exception {
            value: Some("z".repeat(201)),
            ..Default::default()
        };
        let mut e = Event::<'static>::default();
        e.exception.values.push(exc);
        let scrubbed = before_send(e).expect("event must be Some after scrub");
        assert_eq!(
            scrubbed.exception.values[0].value.as_deref(),
            Some("[redacted payload]"),
            "exception value > 200 chars must be redacted"
        );
    }

    #[test]
    fn before_send_retains_allowed_tags() {
        let mut e = Event::<'static>::default();
        e.tags.insert("environment".to_string(), "beta".to_string());
        e.tags.insert("release".to_string(), "v1".to_string());
        e.tags.insert("some_sdk_tag".to_string(), "leak".to_string());
        let scrubbed = before_send(e).expect("event must be Some after scrub");
        assert!(scrubbed.tags.contains_key("environment"));
        assert!(scrubbed.tags.contains_key("release"));
        assert!(
            !scrubbed.tags.contains_key("some_sdk_tag"),
            "non-allow-listed tags must be removed"
        );
    }

    // ── Consent file persistence tests (use tempdir — never touch AppData) ──

    #[test]
    fn consent_load_treats_missing_file_as_absent() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("sentry.txt");
        // File does not exist
        assert_eq!(load_consent_from_path(&path), ConsentState::Absent);
    }

    #[test]
    fn consent_save_and_load_enabled_roundtrip() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("sentry.txt");
        save_consent_to_path(&path, ConsentState::Enabled).expect("save");
        assert_eq!(load_consent_from_path(&path), ConsentState::Enabled);
    }

    #[test]
    fn consent_save_and_load_disabled_roundtrip() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("sentry.txt");
        save_consent_to_path(&path, ConsentState::Disabled).expect("save");
        assert_eq!(load_consent_from_path(&path), ConsentState::Disabled);
    }

    #[test]
    fn consent_save_overwrites_previous_value() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("sentry.txt");
        save_consent_to_path(&path, ConsentState::Enabled).expect("first save");
        save_consent_to_path(&path, ConsentState::Disabled).expect("second save");
        assert_eq!(
            load_consent_from_path(&path),
            ConsentState::Disabled,
            "second write must overwrite first"
        );
    }

    #[test]
    fn consent_file_with_garbage_treats_as_absent() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("sentry.txt");
        std::fs::write(&path, "asdf_garbage_value\n").expect("write garbage");
        assert_eq!(
            load_consent_from_path(&path),
            ConsentState::Absent,
            "unrecognised content must read as Absent"
        );
    }

    #[test]
    fn consent_file_with_leading_whitespace_trims_correctly() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("sentry.txt");
        std::fs::write(&path, "  enabled\n").expect("write");
        assert_eq!(
            load_consent_from_path(&path),
            ConsentState::Enabled,
            "leading/trailing whitespace must be trimmed before matching"
        );
    }

    #[test]
    fn consent_absent_state_save_is_noop() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("sentry.txt");
        save_consent_to_path(&path, ConsentState::Absent).expect("noop must not error");
        assert!(
            !path.exists(),
            "saving Absent must not create the consent file"
        );
    }

    // ── DSN placeholder detection ────────────────────────────────────────────

    #[test]
    fn dsn_placeholder_contains_replace_me_marker() {
        // Ensures init_if_enabled() safely refuses to start Sentry in any build
        // where the user has not yet replaced the placeholder with a real DSN.
        assert!(
            SENTRY_DSN.contains("REPLACE-ME"),
            "SENTRY_DSN must contain REPLACE-ME until the real Vong-owned DSN is pasted in"
        );
    }

    #[test]
    fn init_if_enabled_returns_none_in_test_environment() {
        // Consent file absent in test environment (no AppData/sentry.txt) AND
        // DSN is still placeholder → both gates prevent Sentry starting.
        let result = init_if_enabled();
        assert!(
            result.is_none(),
            "init_if_enabled must return None when consent absent + DSN placeholder"
        );
    }
}
