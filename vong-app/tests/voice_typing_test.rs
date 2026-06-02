//! Tests for voice_typing module — state machine, config roundtrip, sanitizer,
//! and inject_text early-exit contract.
//!
//! vong-app is a binary crate; its internal modules are not importable.
//! We mirror the types and pure-logic functions here instead, following
//! the same pattern as recording_config_test.rs.
//!
//! Tests do NOT require audio hardware, a Whisper model, or OS focus.

// ──────────────────────────────────────────────────────────────────────────────
// Mirrored: VoiceTypingState (state machine)
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum VoiceTypingState {
    Idle,
    Listening { started_at: std::time::Instant },
    Transcribing,
    Injecting,
    Error(String),
}

impl VoiceTypingState {
    fn label(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Listening { .. } => "listening",
            Self::Transcribing => "transcribing",
            Self::Injecting => "injecting",
            Self::Error(_) => "error",
        }
    }

    fn is_active(&self) -> bool {
        matches!(self, Self::Listening { .. } | Self::Transcribing | Self::Injecting)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Mirrored: VoiceTypingConfig
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct VoiceTypingConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_hotkey")]
    hotkey: String,
    #[serde(default = "default_max_duration_secs")]
    max_duration_secs: u32,
    #[serde(default = "default_language")]
    language: String,
}

fn default_hotkey() -> String {
    "Ctrl+Shift+V".to_string()
}
fn default_max_duration_secs() -> u32 {
    10
}
fn default_language() -> String {
    "vi".to_string()
}

impl Default for VoiceTypingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hotkey: default_hotkey(),
            max_duration_secs: default_max_duration_secs(),
            language: default_language(),
        }
    }
}

impl VoiceTypingConfig {
    fn clamp_in_place(&mut self) {
        self.max_duration_secs = self.max_duration_secs.clamp(1, 30);
        if self.language.trim().is_empty() {
            self.language = default_language();
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Mirrored: sanitize_for_injection
// ──────────────────────────────────────────────────────────────────────────────

fn sanitize_for_injection(input: &str) -> String {
    input
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect()
}

// ──────────────────────────────────────────────────────────────────────────────
// Mirrored: inject_text early-exit (empty sanitized string path only)
// ──────────────────────────────────────────────────────────────────────────────

/// Mirrors the "empty after sanitize → Ok(()) without OS call" branch.
fn inject_text_empty_check(text: &str) -> Result<(), String> {
    let sanitized = sanitize_for_injection(text);
    if sanitized.is_empty() {
        return Ok(());
    }
    // Would call enigo here — but we don't in tests (no OS focus needed).
    Err("would call enigo".to_string())
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests: state machine
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn state_idle_label_and_not_active() {
    let s = VoiceTypingState::Idle;
    assert_eq!(s.label(), "idle");
    assert!(!s.is_active());
}

#[test]
fn state_listening_label_and_is_active() {
    let s = VoiceTypingState::Listening {
        started_at: std::time::Instant::now(),
    };
    assert_eq!(s.label(), "listening");
    assert!(s.is_active());
}

#[test]
fn state_transcribing_label_and_is_active() {
    let s = VoiceTypingState::Transcribing;
    assert_eq!(s.label(), "transcribing");
    assert!(s.is_active());
}

#[test]
fn state_injecting_label_and_is_active() {
    let s = VoiceTypingState::Injecting;
    assert_eq!(s.label(), "injecting");
    assert!(s.is_active());
}

#[test]
fn state_error_label_and_not_active() {
    let s = VoiceTypingState::Error("mic denied".to_string());
    assert_eq!(s.label(), "error");
    assert!(!s.is_active(), "Error state must not be considered active");
}

#[test]
fn state_transitions_idle_to_listening_to_idle() {
    // Simulate the hotkey-press → cancel second-press flow.
    let mut state = VoiceTypingState::Idle;
    assert!(!state.is_active());

    state = VoiceTypingState::Listening {
        started_at: std::time::Instant::now(),
    };
    assert!(state.is_active());

    // Second hotkey press cancels → back to Idle.
    state = VoiceTypingState::Idle;
    assert!(!state.is_active());
    assert_eq!(state.label(), "idle");
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests: VoiceTypingConfig JSON roundtrip
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn config_default_is_disabled() {
    let cfg = VoiceTypingConfig::default();
    assert!(!cfg.enabled, "default must be OFF for user safety");
    assert_eq!(cfg.hotkey, "Ctrl+Shift+V");
    assert_eq!(cfg.max_duration_secs, 10);
    assert_eq!(cfg.language, "vi");
}

#[test]
fn config_json_roundtrip_preserves_all_fields() {
    let original = VoiceTypingConfig {
        enabled: true,
        hotkey: "Ctrl+Shift+V".to_string(),
        max_duration_secs: 20,
        language: "en".to_string(),
    };
    let json = serde_json::to_string(&original).expect("serialize");
    let restored: VoiceTypingConfig = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(restored, original);
}

#[test]
fn config_missing_fields_fall_back_to_defaults() {
    // Simulates loading an older recording.json that has no voice_typing key.
    let json = r#"{}"#;
    let cfg: VoiceTypingConfig = serde_json::from_str(json).expect("deserialize partial");
    assert!(!cfg.enabled);
    assert_eq!(cfg.hotkey, "Ctrl+Shift+V");
    assert_eq!(cfg.max_duration_secs, 10);
    assert_eq!(cfg.language, "vi");
}

#[test]
fn config_clamp_max_duration_to_bounds() {
    let mut cfg = VoiceTypingConfig {
        enabled: false,
        hotkey: "Ctrl+Shift+V".to_string(),
        max_duration_secs: 99,
        language: "vi".to_string(),
    };
    cfg.clamp_in_place();
    assert_eq!(cfg.max_duration_secs, 30, "clamped to max 30");

    cfg.max_duration_secs = 0;
    cfg.clamp_in_place();
    assert_eq!(cfg.max_duration_secs, 1, "clamped to min 1");
}

#[test]
fn config_hotkey_string_persists_unchanged() {
    // Future rebind support: hotkey is stored as-is regardless of Sprint 5 changes.
    let cfg = VoiceTypingConfig {
        enabled: false,
        hotkey: "Ctrl+Alt+T".to_string(),
        max_duration_secs: 5,
        language: "ja".to_string(),
    };
    let json = serde_json::to_string(&cfg).expect("serialize");
    assert!(json.contains("Ctrl+Alt+T"), "hotkey string must survive roundtrip");
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests: text sanitizer
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn sanitize_strips_nul_esc_bs() {
    let input = "hello\0world\x1bbye\x08back";
    let out = sanitize_for_injection(input);
    assert!(!out.contains('\0'), "NUL must be stripped");
    assert!(!out.contains('\x1b'), "ESC must be stripped");
    assert!(!out.contains('\x08'), "BS must be stripped");
    assert!(out.contains("hello") && out.contains("world") && out.contains("bye"),
        "printable ASCII must be preserved");
}

#[test]
fn sanitize_keeps_newlines_and_tabs() {
    let input = "line1\nline2\ttabbed";
    let out = sanitize_for_injection(input);
    assert!(out.contains('\n'), "\\n must be preserved");
    assert!(out.contains('\t'), "\\t must be preserved");
}

#[test]
fn sanitize_keeps_unicode_and_vietnamese() {
    let input = "Xin chào bạn 🎉 日本語 한국어 中文 không";
    let out = sanitize_for_injection(input);
    assert_eq!(out, input, "all valid Unicode must pass through unchanged");
}

#[test]
fn sanitize_empty_input_returns_empty() {
    assert_eq!(sanitize_for_injection(""), "");
}

#[test]
fn sanitize_all_c0_controls_stripped_except_newline_and_tab() {
    // Build string of C0 controls excluding \n and \t (which are explicitly allowed).
    let controls: String = (1u8..32u8)
        .filter(|&b| b != b'\n' && b != b'\t')
        .map(|b| b as char)
        .collect();
    let out = sanitize_for_injection(&controls);
    // Everything remaining must be either \n, \t, or non-control.
    for c in out.chars() {
        assert!(
            !c.is_control() || c == '\n' || c == '\t',
            "unexpected control char U+{:04X} survived sanitization",
            c as u32
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests: inject_text contract (no real OS injection — early-exit path only)
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn inject_text_returns_ok_when_sanitized_to_empty() {
    // A string composed only of stripped control chars sanitizes to empty.
    // The real inject_text returns Ok(()) early without calling enigo.
    // This mirrors that invariant via the local helper.
    let only_controls = "\0\x1b\x08\x07\x0e\x0f";
    let result = inject_text_empty_check(only_controls);
    assert!(result.is_ok(), "empty post-sanitize must short-circuit to Ok");
}

#[test]
fn inject_text_proceeds_for_printable_text() {
    // A printable string does not hit the early return.
    // The local helper returns Err("would call enigo") to signal it proceeded.
    let result = inject_text_empty_check("xin chào");
    assert!(
        result.is_err(),
        "printable text must pass sanitization and proceed to injection step"
    );
    assert_eq!(result.unwrap_err(), "would call enigo");
}
