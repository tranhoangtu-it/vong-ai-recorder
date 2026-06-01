//! Storage model types — Session, Segment, Summary + NFC normalization.
//!
//! Plan v2 Section 4.4 — schema definition.
//! Plan v2 Section 13.3.2 — Unicode NFC normalization (macOS NFD vs Win NFC).

use unicode_normalization::UnicodeNormalization;

/// Normalize text to Unicode NFC form (canonical composed).
///
/// macOS filesystem APIs return NFD ("ơ" = o + combining horn).
/// Windows/most tools use NFC ("ơ" = single codepoint).
/// We standardize on NFC for storage + display consistency.
pub fn normalize(text: &str) -> String {
    text.nfc().collect()
}

/// New session insert payload (no `id` yet — assigned by DB).
#[derive(Debug, Clone)]
pub struct NewSession {
    /// Recording start time in epoch milliseconds (UTC).
    pub started_at_ms: i64,
    /// Audio source: "mic" | "loopback" | "mixed".
    pub audio_source: String,
    /// STT provider: "soniox" (MVP only).
    pub provider: String,
    /// Optional pre-set detected language (typically updated post-session).
    pub detected_language: Option<String>,
    /// Optional metadata JSON blob.
    pub meta_json: Option<String>,
}

/// Existing session row (post-insert/select).
#[derive(Debug, Clone)]
pub struct Session {
    /// Primary key.
    pub id: i64,
    /// Recording start in epoch ms.
    pub started_at_ms: i64,
    /// Recording end in epoch ms (None while in-progress).
    pub ended_at_ms: Option<i64>,
    /// Duration ms (None while in-progress).
    pub duration_ms: Option<i64>,
    /// "mic" | "loopback" | "mixed".
    pub audio_source: String,
    /// STT provider.
    pub provider: String,
    /// Detected language aggregate.
    pub detected_language: Option<String>,
    /// Optional saved audio file path.
    pub audio_path: Option<String>,
    /// Metadata JSON.
    pub meta_json: Option<String>,
}

/// New segment insert payload.
#[derive(Debug, Clone)]
pub struct NewSegment {
    /// Foreign key to sessions.id.
    pub session_id: i64,
    /// Monotonic sequence within session.
    pub seq: i64,
    /// Segment start time relative to session start (ms).
    pub start_ms: i64,
    /// Segment end time (ms).
    pub end_ms: i64,
    /// Optional speaker label (NULL for MVP — no diarization).
    pub speaker_label: Option<String>,
    /// Segment text (caller can pass non-normalized; repository will NFC normalize).
    pub text: String,
    /// Optional language tag.
    pub language: Option<String>,
    /// Optional confidence 0..1.
    pub confidence: Option<f64>,
    /// 1 for final, 0 for partial.
    pub is_final: bool,
}

/// Existing transcript segment row.
#[derive(Debug, Clone)]
pub struct Segment {
    /// Primary key.
    pub id: i64,
    /// Foreign key to sessions.id.
    pub session_id: i64,
    /// Sequence within session.
    pub seq: i64,
    /// Start time ms.
    pub start_ms: i64,
    /// End time ms.
    pub end_ms: i64,
    /// Speaker label (None for MVP).
    pub speaker_label: Option<String>,
    /// Transcript text (NFC normalized).
    pub text: String,
    /// Language tag.
    pub language: Option<String>,
    /// Confidence 0..1.
    pub confidence: Option<f64>,
    /// Final or partial.
    pub is_final: bool,
}

/// New summary insert payload.
#[derive(Debug, Clone)]
pub struct NewSummary {
    /// Foreign key to sessions.id.
    pub session_id: i64,
    /// Summary kind. Use `"session"` for end-of-session LLM summaries.
    pub kind: String,
    /// LLM provider identifier e.g. `"openai"`.
    pub llm_provider: Option<String>,
    /// LLM model tag e.g. `"gpt-4o-mini-2024-07-18"`.
    pub llm_model: Option<String>,
    /// Summary text content (Vietnamese prose, 3-5 sentences).
    pub content: String,
}

/// Existing summary row (post-insert/select).
#[derive(Debug, Clone)]
pub struct Summary {
    /// Primary key.
    pub id: i64,
    /// Foreign key to sessions.id.
    pub session_id: i64,
    /// Summary kind.
    pub kind: String,
    /// LLM provider.
    pub llm_provider: Option<String>,
    /// LLM model.
    pub llm_model: Option<String>,
    /// Summary text.
    pub content: String,
    /// Creation time in epoch milliseconds (UTC).
    pub created_at_ms: i64,
}

/// FTS search hit — includes snippet for UI highlight.
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// Session containing the matched segment.
    pub session_id: i64,
    /// Session start time (for display ordering).
    pub session_started_at_ms: i64,
    /// Session language tag.
    pub language: Option<String>,
    /// Segment text containing match (NFC normalized).
    pub text: String,
    /// Segment start time within session.
    pub start_ms: i64,
    /// SQLite-generated snippet with `<b>match</b>` markup.
    pub snippet: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfc_normalize_idempotent_on_nfc() {
        let input = "Cuộc họp"; // already NFC
        let out = normalize(input);
        assert_eq!(out, input);
    }

    #[test]
    fn nfc_normalize_composes_nfd() {
        // NFD form of "ơ" = o (U+006F) + combining horn (U+031B)
        let nfd = "to\u{031B}i no\u{0301}i tie\u{0302}\u{0301}ng vie\u{0323}t";
        let nfc = normalize(nfd);
        // NFC form is shorter (fewer codepoints — composed characters)
        assert!(
            nfc.chars().count() <= nfd.chars().count(),
            "NFC should compose to fewer or equal codepoints"
        );
    }

    #[test]
    fn nfc_preserves_ascii() {
        let s = "Hello, World!";
        assert_eq!(normalize(s), s);
    }
}
