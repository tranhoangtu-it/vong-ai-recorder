//! Recording settings persistence — VAD slider values + active Whisper model.
//!
//! Saved to `%APPDATA%\Vong\Vong AI Recorder\config\recording.json`.
//!
//! Format:
//! ```json
//! {
//!   "version": 1,
//!   "vad": { "threshold": 0.65, "hangover_ms": 200, "max_duration_ms": 8000 },
//!   "whisper_model_path": "C:\\...\\models\\ggml-base.bin"
//! }
//! ```
//!
//! Unknown/extra fields are ignored. Missing fields use defaults. Out-of-range
//! values are clamped and logged.

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use vong_audio::VadConfig;

// ──────────────────────────────────────────────────────────────────────────
// Types
// ──────────────────────────────────────────────────────────────────────────

/// Serializable snapshot of user-tunable VAD parameters.
///
/// Intentionally a separate struct from `VadConfig` so the persistence layer
/// never accidentally serializes `partial_emit_ms` (which is hardcoded in
/// VadConfig and skipped via `#[serde(skip)]` there — but having the explicit
/// subset here makes the contract obvious).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VadParams {
    /// Voice detection threshold [0.30 – 0.95]. Default 0.65.
    pub threshold: f32,
    /// Silence hangover in milliseconds [100 – 800]. Default 200.
    pub hangover_ms: u32,
    /// Force-pack ceiling in milliseconds [3000 – 15000]. Default 8000.
    pub max_duration_ms: u32,
}

impl Default for VadParams {
    fn default() -> Self {
        let d = VadConfig::default();
        Self {
            threshold: d.threshold,
            hangover_ms: d.hangover_ms,
            max_duration_ms: d.max_duration_ms,
        }
    }
}

impl VadParams {
    /// Clamp all fields to their valid UI ranges in-place.
    /// Called after deserialization to guard against hand-edited or corrupt JSON.
    pub fn clamp_in_place(&mut self) {
        self.threshold = self.threshold.clamp(0.30, 0.95);
        self.hangover_ms = self.hangover_ms.clamp(100, 800);
        self.max_duration_ms = self.max_duration_ms.clamp(3000, 15000);
    }
}

/// Voice Typing sub-configuration embedded in `RecordingConfig`.
///
/// Persisted inside `recording.json` under the `"voice_typing"` key.
/// Default OFF — user must opt in explicitly (text injection is sensitive).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceTypingConfig {
    /// Master toggle. `false` (default) = hotkey is ignored, no injection occurs.
    #[serde(default)]
    pub enabled: bool,
    /// Hotkey as a stable string representation (e.g. "Ctrl+Shift+V").
    /// Stored even though rebind is deferred to Sprint 5 — future-proofs the
    /// config format without forcing a migration.
    #[serde(default = "default_voice_typing_hotkey")]
    pub hotkey: String,
    /// Maximum recording length in seconds before force-pack [1 – 30]. Default 10.
    #[serde(default = "default_voice_typing_max_duration_secs")]
    pub max_duration_secs: u32,
    /// Source language hint for Whisper (ISO 639-1). Default "vi".
    #[serde(default = "default_voice_typing_language")]
    pub language: String,
}

fn default_voice_typing_hotkey() -> String {
    "Ctrl+Shift+V".to_string()
}

fn default_voice_typing_max_duration_secs() -> u32 {
    10
}

fn default_voice_typing_language() -> String {
    "vi".to_string()
}

impl Default for VoiceTypingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hotkey: default_voice_typing_hotkey(),
            max_duration_secs: default_voice_typing_max_duration_secs(),
            language: default_voice_typing_language(),
        }
    }
}

impl VoiceTypingConfig {
    /// Clamp `max_duration_secs` to [1, 30]. Called after deserialization.
    pub fn clamp_in_place(&mut self) {
        self.max_duration_secs = self.max_duration_secs.clamp(1, 30);
        if self.language.trim().is_empty() {
            self.language = default_voice_typing_language();
        }
    }
}

/// Root recording configuration persisted to `recording.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingConfig {
    /// Schema version for forward-compat. Always written as 1; unknown versions
    /// fall through to defaults gracefully via `#[serde(default)]`.
    #[serde(default = "default_version")]
    pub version: u32,
    /// User-tunable VAD parameters.
    #[serde(default)]
    pub vad: VadParams,
    /// Full path to the active Whisper model file. `None` = use the default
    /// search ladder in `WhisperLocalProvider::resolve_default_model_path()`.
    #[serde(default)]
    pub whisper_model_path: Option<PathBuf>,
    /// Voice Typing feature configuration.
    #[serde(default)]
    pub voice_typing: VoiceTypingConfig,
}

fn default_version() -> u32 {
    1
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            version: 1,
            vad: VadParams::default(),
            whisper_model_path: None,
            voice_typing: VoiceTypingConfig::default(),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Path resolution
// ──────────────────────────────────────────────────────────────────────────

/// Resolve the config file path.
/// Returns `None` if the platform doesn't support `ProjectDirs` (shouldn't
/// happen on Windows, but defensive).
pub fn config_path() -> Option<PathBuf> {
    ProjectDirs::from("com", "Vong", "Vong AI Recorder")
        .map(|d| d.config_dir().join("recording.json"))
}

// ──────────────────────────────────────────────────────────────────────────
// Load / Save
// ──────────────────────────────────────────────────────────────────────────

/// Load `recording.json`. Falls back to `RecordingConfig::default()` on any
/// error (file absent, corrupt JSON, invalid values). Never panics.
pub fn load() -> RecordingConfig {
    let Some(path) = config_path() else {
        tracing::debug!("recording config: ProjectDirs unavailable — using defaults");
        return RecordingConfig::default();
    };

    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {
            // First launch — no file yet; silently use defaults.
            return RecordingConfig::default();
        }
        Err(e) => {
            tracing::warn!(error = %e, "recording.json read failed — using defaults");
            return RecordingConfig::default();
        }
    };

    match serde_json::from_str::<RecordingConfig>(&content) {
        Ok(mut cfg) => {
            cfg.vad.clamp_in_place();
            cfg.voice_typing.clamp_in_place();
            tracing::debug!(
                threshold = cfg.vad.threshold,
                hangover_ms = cfg.vad.hangover_ms,
                max_duration_ms = cfg.vad.max_duration_ms,
                vt_enabled = cfg.voice_typing.enabled,
                vt_max_secs = cfg.voice_typing.max_duration_secs,
                "recording.json loaded"
            );
            cfg
        }
        Err(e) => {
            tracing::warn!(error = %e, "recording.json parse failed — using defaults");
            RecordingConfig::default()
        }
    }
}

/// Atomically save `RecordingConfig` to disk.
///
/// Writes to a `.tmp` file first, then renames over the target — ensures no
/// torn writes on power loss mid-debounce. The parent directory is created if
/// it doesn't exist (first launch).
pub fn save(cfg: &RecordingConfig) -> std::io::Result<()> {
    let path = config_path()
        .ok_or_else(|| std::io::Error::other("ProjectDirs unavailable"))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(cfg)
        .map_err(|e| std::io::Error::other(format!("serialize recording config: {e}")))?;
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, &path)?;
    tracing::debug!(
        threshold = cfg.vad.threshold,
        hangover_ms = cfg.vad.hangover_ms,
        max_duration_ms = cfg.vad.max_duration_ms,
        "recording.json saved"
    );
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────
// Conversion helpers
// ──────────────────────────────────────────────────────────────────────────

/// Convert `VadParams` into a full `VadConfig`, preserving all protocol-constant
/// fields (`pre_roll_ms`, `min_duration_ms`, `partial_emit_ms`) from the
/// `VadConfig::default()` baseline.
///
/// `partial_emit_ms` is NOT taken from `params` — it is a fixed protocol
/// constant (1500 ms) tied to earshot's tokenization and is never exposed to
/// the UI.
pub fn into_vad_config(params: &VadParams) -> VadConfig {
    let d = VadConfig::default();
    VadConfig {
        threshold: params.threshold,
        hangover_ms: params.hangover_ms,
        max_duration_ms: params.max_duration_ms,
        pre_roll_ms: d.pre_roll_ms,
        min_duration_ms: d.min_duration_ms,
        partial_emit_ms: d.partial_emit_ms,
    }
}
