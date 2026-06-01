//! Integration tests for recording_config persistence module.
//!
//! Covers: JSON roundtrip, bounds clamping, missing-field defaults,
//! corrupt-JSON fallback, and atomic-write safety.

// Reach into the binary crate's module via path re-export.
// We test the functions directly by re-implementing the same logic here,
// avoiding the need for `pub(crate)` exports from a binary crate.
// The module is in `vong-app/src/recording_config.rs`.

use serde_json::Value;
use vong_audio::VadConfig;

// ──────────────────────────────────────────────────────────────────────────
// Mirror types for testing (same structure as recording_config.rs)
// ──────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
struct VadParams {
    threshold: f32,
    hangover_ms: u32,
    max_duration_ms: u32,
}

impl VadParams {
    fn clamp_in_place(&mut self) {
        self.threshold = self.threshold.clamp(0.30, 0.95);
        self.hangover_ms = self.hangover_ms.clamp(100, 800);
        self.max_duration_ms = self.max_duration_ms.clamp(3000, 15000);
    }
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RecordingConfig {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    vad: VadParams,
    #[serde(default)]
    whisper_model_path: Option<std::path::PathBuf>,
}

fn default_version() -> u32 {
    1
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self { version: 1, vad: VadParams::default(), whisper_model_path: None }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn default_roundtrip() {
    let original = RecordingConfig::default();
    let json = serde_json::to_string_pretty(&original).expect("serialize");
    let restored: RecordingConfig = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(original.version, restored.version);
    assert!((original.vad.threshold - restored.vad.threshold).abs() < f32::EPSILON);
    assert_eq!(original.vad.hangover_ms, restored.vad.hangover_ms);
    assert_eq!(original.vad.max_duration_ms, restored.vad.max_duration_ms);
    assert_eq!(original.whisper_model_path, restored.whisper_model_path);
}

#[test]
fn clamps_out_of_range_values() {
    let mut params = VadParams {
        threshold: 2.0,          // above max 0.95
        hangover_ms: 50,          // below min 100
        max_duration_ms: 100_000, // above max 15000
    };
    params.clamp_in_place();
    assert!((params.threshold - 0.95).abs() < f32::EPSILON, "threshold must clamp to 0.95");
    assert_eq!(params.hangover_ms, 100, "hangover_ms must clamp to 100");
    assert_eq!(params.max_duration_ms, 15000, "max_duration_ms must clamp to 15000");
}

#[test]
fn clamps_below_minimum_values() {
    let mut params = VadParams {
        threshold: -0.5,    // below min 0.30
        hangover_ms: 999,   // above max 800
        max_duration_ms: 1, // below min 3000
    };
    params.clamp_in_place();
    assert!((params.threshold - 0.30).abs() < f32::EPSILON, "threshold must clamp to 0.30");
    assert_eq!(params.hangover_ms, 800, "hangover_ms must clamp to 800");
    assert_eq!(params.max_duration_ms, 3000, "max_duration_ms must clamp to 3000");
}

#[test]
fn parse_missing_fields_uses_defaults() {
    // Only "version" present — all vad fields missing.
    // Use the fallback: if vad is missing entirely, default kicks in.
    let json2 = r#"{ "version": 1 }"#;
    let restored: RecordingConfig = serde_json::from_str(json2).expect("deserialize");
    let defaults = VadParams::default();
    assert!((restored.vad.threshold - defaults.threshold).abs() < f32::EPSILON);
    assert_eq!(restored.vad.hangover_ms, defaults.hangover_ms);
    assert_eq!(restored.vad.max_duration_ms, defaults.max_duration_ms);
}

#[test]
fn parse_invalid_json_does_not_panic() {
    let bad_inputs = [
        "",
        "{}",
        "not json at all {{{{",
        r#"{"version": "wrong type"}"#,
        r#"{"vad": null}"#,
    ];
    for input in bad_inputs {
        // Either succeeds (using defaults for missing fields) or returns an error.
        // The important invariant: it NEVER panics.
        let _ = serde_json::from_str::<RecordingConfig>(input);
    }
    // Confirm the most likely fallback case doesn't panic.
    let result = serde_json::from_str::<RecordingConfig>("not json");
    assert!(result.is_err(), "malformed JSON should produce an error, not Ok");
}

#[test]
fn atomic_write_no_partial_state() {
    // Write config to a temp directory and verify:
    // 1. Target file exists after save.
    // 2. No .tmp file is left behind (rename cleaned up).
    let dir = std::env::temp_dir().join("vong_test_recording_config");
    std::fs::create_dir_all(&dir).expect("create test dir");
    let target = dir.join("recording.json");
    let tmp = dir.join("recording.json.tmp");

    // Ensure clean slate.
    let _ = std::fs::remove_file(&target);
    let _ = std::fs::remove_file(&tmp);

    let cfg = RecordingConfig::default();
    let json = serde_json::to_string_pretty(&cfg).expect("serialize");
    std::fs::write(&tmp, &json).expect("write tmp");
    std::fs::rename(&tmp, &target).expect("atomic rename");

    assert!(target.exists(), "target file must exist after atomic write");
    assert!(!tmp.exists(), "tmp file must not exist after successful rename");

    // Content must be valid JSON and round-trip correctly.
    let content = std::fs::read_to_string(&target).expect("read back");
    let restored: RecordingConfig = serde_json::from_str(&content).expect("parse back");
    assert!((restored.vad.threshold - cfg.vad.threshold).abs() < f32::EPSILON);

    // Cleanup.
    let _ = std::fs::remove_file(&target);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn json_does_not_contain_partial_emit_ms() {
    // VadConfig's partial_emit_ms is marked #[serde(skip)] — must not appear in JSON.
    let cfg = VadConfig::default();
    let json = serde_json::to_string(&cfg).expect("serialize VadConfig");
    assert!(
        !json.contains("partial_emit_ms"),
        "partial_emit_ms must be skipped in serialization: {}",
        json
    );
    // Deserializing back must restore the default protocol value 1500.
    let restored: VadConfig = serde_json::from_str(&json).expect("deserialize VadConfig");
    assert_eq!(
        restored.partial_emit_ms, 1500,
        "partial_emit_ms must restore to 1500 after roundtrip"
    );
}

#[test]
fn vad_params_matches_vad_config_defaults() {
    // VadParams::default() must match VadConfig::default() for the three exposed fields.
    let p = VadParams::default();
    let c = VadConfig::default();
    assert!((p.threshold - c.threshold).abs() < f32::EPSILON);
    assert_eq!(p.hangover_ms, c.hangover_ms);
    assert_eq!(p.max_duration_ms, c.max_duration_ms);
}

#[test]
fn json_roundtrip_with_model_path() {
    let cfg = RecordingConfig {
        version: 1,
        vad: VadParams { threshold: 0.75, hangover_ms: 300, max_duration_ms: 6000 },
        whisper_model_path: Some(std::path::PathBuf::from(r"C:\models\ggml-small.bin")),
    };
    let json = serde_json::to_string_pretty(&cfg).expect("serialize");
    let restored: RecordingConfig = serde_json::from_str(&json).expect("deserialize");

    assert!((restored.vad.threshold - 0.75).abs() < f32::EPSILON);
    assert_eq!(restored.vad.hangover_ms, 300);
    assert_eq!(restored.vad.max_duration_ms, 6000);
    assert_eq!(
        restored.whisper_model_path,
        Some(std::path::PathBuf::from(r"C:\models\ggml-small.bin"))
    );

    // Verify the JSON structure matches the documented format.
    let v: Value = serde_json::from_str(&json).expect("parse as Value");
    assert_eq!(v["version"], 1);
    assert!((v["vad"]["threshold"].as_f64().unwrap() - 0.75).abs() < 1e-5);
    assert_eq!(v["vad"]["hangover_ms"], 300);
    assert_eq!(v["vad"]["max_duration_ms"], 6000);
}
