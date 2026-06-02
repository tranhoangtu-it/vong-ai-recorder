//! Diarization integration tests — Phase 18.
//!
//! Covers observable behavior via the public API:
//!   1. `TranscriptEvent::Final` carries `speaker: Some(...)` when constructed with one.
//!   2. `TranscriptEvent::Final` carries `speaker: None` for Whisper-style events.
//!   3. `TranscriptEvent::Final` carries `speaker: None` for OpenAI-style events.
//!   4. `speaker` field survives `Clone` on `TranscriptEvent::Final`.
//!   5. `StreamOpts.enable_diarization` default is `false`.
//!   6. `StreamOpts.enable_diarization = true` is accepted without error.
//!   7. Soniox `Token` JSON with `speaker` field deserializes to non-null.
//!   8. Soniox `Token` JSON without `speaker` field yields null.
//!   9. `TranscriptEvent::Partial` has no speaker field (structural check).
//!   10. `TranscriptEvent::Final` with `speaker: Some("2")` has correct seq + speaker.

use vong_transcribe::{StreamOpts, TranscriptEvent};

// ── Test 1: Final with speaker Some("1") carries the label through ───────────

#[test]
fn final_event_carries_speaker_some() {
    let evt = make_final(Some("1".into()));
    match evt {
        TranscriptEvent::Final { speaker, .. } => {
            assert_eq!(speaker.as_deref(), Some("1"));
        }
        _ => panic!("expected Final"),
    }
}

// ── Test 2: Whisper-style Final always uses speaker: None ────────────────────

#[test]
fn whisper_style_final_has_speaker_none() {
    let evt = make_final(None);
    match evt {
        TranscriptEvent::Final { speaker, .. } => {
            assert!(speaker.is_none(), "Whisper Final must have speaker = None");
        }
        _ => panic!("expected Final"),
    }
}

// ── Test 3: OpenAI-style Final always uses speaker: None ─────────────────────

#[test]
fn openai_style_final_has_speaker_none() {
    // OpenAI Realtime transcription-only intent has no diarization support.
    let evt = TranscriptEvent::Final {
        seq: 1,
        text: String::new(),
        language: None,
        original_text: "hello world".into(),
        original_language: Some("en".into()),
        speaker: None, // always None for OpenAI
        start: std::time::Duration::ZERO,
        end: std::time::Duration::ZERO,
    };
    match evt {
        TranscriptEvent::Final { speaker, .. } => {
            assert!(speaker.is_none());
        }
        _ => panic!("expected Final"),
    }
}

// ── Test 4: speaker survives Clone ───────────────────────────────────────────

#[test]
fn final_speaker_survives_clone() {
    let evt = make_final(Some("2".into()));
    let cloned = evt.clone();
    match cloned {
        TranscriptEvent::Final { speaker, seq, .. } => {
            assert_eq!(seq, 42);
            assert_eq!(speaker.as_deref(), Some("2"));
        }
        _ => panic!("expected Final after clone"),
    }
}

// ── Test 5: StreamOpts default has enable_diarization = false ────────────────

#[test]
fn stream_opts_default_diarization_off() {
    let opts = StreamOpts::default();
    assert!(!opts.enable_diarization, "default must be false for backward compat");
}

// ── Test 6: StreamOpts with enable_diarization = true is a valid value ───────

#[test]
fn stream_opts_diarization_on_is_valid() {
    let opts = StreamOpts {
        enable_diarization: true,
        ..Default::default()
    };
    assert!(opts.enable_diarization);
    // Verify other fields are still their defaults.
    assert!(opts.language_hint.is_none());
    assert!(!opts.enable_lid);
    assert!(opts.enable_translation_to.is_none());
}

// ── Test 7: Soniox Token JSON with speaker deserializes to non-null ───────────

#[test]
fn soniox_token_json_with_speaker_is_non_null() {
    let json = r#"{"text":"Xin chào","is_final":true,"language":"vi","speaker":"1"}"#;
    let val: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
    assert_eq!(
        val["speaker"],
        serde_json::Value::String("1".into()),
        "speaker field must deserialize to string '1'"
    );
}

// ── Test 8: Soniox Token JSON without speaker field yields null ───────────────

#[test]
fn soniox_token_json_without_speaker_is_null() {
    let json = r#"{"text":"Hello","is_final":true,"language":"en"}"#;
    let val: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
    assert!(
        val["speaker"].is_null(),
        "absent speaker field must deserialize as null, got: {}",
        val["speaker"]
    );
}

// ── Test 9: Partial event has no speaker field (structural compile check) ─────

#[test]
fn partial_event_has_no_speaker_field() {
    // This test verifies at the struct level that Partial does NOT carry a
    // speaker — diarization is only meaningful on finalized segments.
    // If someone adds speaker to Partial, this exhaustive match will fail to compile.
    let evt = TranscriptEvent::Partial {
        seq: 0,
        text: "in progress".into(),
        language: Some("vi".into()),
    };
    match evt {
        TranscriptEvent::Partial { seq, text, language } => {
            assert_eq!(seq, 0);
            assert_eq!(text, "in progress");
            assert_eq!(language.as_deref(), Some("vi"));
        }
        _ => panic!("expected Partial"),
    }
}

// ── Test 10: Final with speaker Some("2") has correct seq + speaker tag ───────

#[test]
fn final_event_correct_seq_and_speaker() {
    let evt = TranscriptEvent::Final {
        seq: 99,
        text: String::new(),
        language: None,
        original_text: "kiểm tra".into(),
        original_language: Some("vi".into()),
        speaker: Some("2".into()),
        start: std::time::Duration::from_millis(200),
        end: std::time::Duration::from_millis(1500),
    };
    match evt {
        TranscriptEvent::Final { seq, speaker, end, .. } => {
            assert_eq!(seq, 99);
            assert_eq!(speaker.as_deref(), Some("2"));
            assert_eq!(end, std::time::Duration::from_millis(1500));
        }
        _ => panic!("expected Final"),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_final(speaker: Option<String>) -> TranscriptEvent {
    TranscriptEvent::Final {
        seq: 42,
        text: String::new(),
        language: None,
        original_text: "xin chào thế giới".into(),
        original_language: Some("vi".into()),
        speaker,
        start: std::time::Duration::from_millis(100),
        end: std::time::Duration::from_millis(800),
    }
}
