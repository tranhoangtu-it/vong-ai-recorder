//! STT provider trait abstraction.
//!
//! Plan v2 Section 4.3 — Strategy pattern dual signature (batch + streaming).
//! MVP 0.1 ships only `StreamingTranscriber` (Soniox). Batch trait deferred
//! to MVP 0.2 (OpenAI Whisper-1 REST + AssemblyAI batch).

use crate::error::SttError;
use crate::events::TranscriptEvent;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use vong_audio::Utterance;

// ────────────────────────────────────────────────────────────────────────────
// Dictionary types (shared by all providers + vong-app)
// ────────────────────────────────────────────────────────────────────────────

/// Maximum number of dictionary entries the user can store.
pub const MAX_DICTIONARY_ENTRIES: usize = 100;

/// Maximum byte length per phrase.
pub const PHRASE_MAX_BYTES: usize = 240; // 80 Unicode chars × 3 bytes worst-case

/// Contextual category for a vocabulary entry.
///
/// Used to guide future per-category prioritization (Sprint 3). For Sprint 2
/// all entries are treated equally in prompt-building — the category is stored
/// and round-trips through JSON but does not change injection order.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DictContext {
    /// General vocabulary / common domain terms.
    #[default]
    Common,
    /// Proper names (people, places, products).
    Names,
    /// Technical or specialist terminology.
    Technical,
}

/// A single user-defined vocabulary entry.
///
/// Privacy: phrase content is user data — NEVER write it to tracing events.
/// Only `entry_count` integers are safe to log.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DictEntry {
    /// The phrase or term to hint to the STT engine (e.g. "Vọng AI Recorder").
    pub phrase: String,
    /// Contextual category; used for display and future prioritization.
    pub context: DictContext,
}

// ────────────────────────────────────────────────────────────────────────────
// Dictionary prompt builders
// ────────────────────────────────────────────────────────────────────────────

/// Build the Whisper `initial_prompt` from a dictionary.
///
/// Selects longest phrases first (highest impact per token), then by
/// insertion order. Stops when the accumulated token estimate reaches
/// ~200 tokens (well under Whisper's 224-token `n_prompt_max` ceiling).
/// Returns an empty string when the dictionary is empty.
///
/// Privacy: never call `tracing::*` with the returned string.
pub fn build_whisper_prompt(entries: &[DictEntry]) -> String {
    // Sort longest-first; stable secondary by original index.
    let mut indexed: Vec<(usize, &DictEntry)> = entries.iter().enumerate().collect();
    indexed.sort_by(|a, b| {
        b.1.phrase
            .len()
            .cmp(&a.1.phrase.len())
            .then_with(|| a.0.cmp(&b.0))
    });

    // Conservative heuristic: ~3 bytes/token for Vietnamese (diacritics inflate
    // UTF-8 bytes beyond BPE token count). +2 tokens for separator overhead.
    const TARGET_TOKENS: usize = 200;
    let mut prompt = String::new();
    let mut approx_tokens: usize = 0;

    for (_, entry) in &indexed {
        let term_tokens = (entry.phrase.len() / 3) + 2;
        if approx_tokens + term_tokens > TARGET_TOKENS {
            break;
        }
        if !prompt.is_empty() {
            prompt.push_str(", ");
        }
        prompt.push_str(&entry.phrase);
        approx_tokens += term_tokens;
    }
    prompt
}

/// Build the Soniox `context.terms` list from a dictionary.
///
/// Selects longest phrases first, capped at 8,000 total characters
/// (well under Soniox's 10,000-char budget for the `context` object).
pub fn build_soniox_terms(entries: &[DictEntry]) -> Vec<String> {
    let mut sorted: Vec<&DictEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.phrase.len()));

    const TARGET_CHARS: usize = 8_000;
    let mut total: usize = 0;
    let mut out = Vec::with_capacity(entries.len().min(MAX_DICTIONARY_ENTRIES));
    for entry in sorted {
        let cost = entry.phrase.len() + 1; // +1 for separator
        if total + cost > TARGET_CHARS {
            break;
        }
        out.push(entry.phrase.clone());
        total += cost;
    }
    out
}

/// Build the OpenAI Realtime `session.update.instructions` string from a dictionary.
///
/// Uses a Vietnamese-friendly prefix and caps the total string at 2,000 chars.
/// Returns an empty string when the dictionary is empty (no-op for OpenAI).
pub fn build_openai_instructions(entries: &[DictEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }

    let mut sorted: Vec<&DictEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.phrase.len()));

    // Reserve ~200 chars for the prefix sentence; the rest for the term list.
    const MAX_TERMS_CHARS: usize = 1_800;
    let mut quoted = String::new();
    for (i, entry) in sorted.iter().enumerate() {
        let escaped = entry.phrase.replace('"', "\\\"");
        let next = if i == 0 {
            format!("\"{escaped}\"")
        } else {
            format!(", \"{escaped}\"")
        };
        if quoted.len() + next.len() > MAX_TERMS_CHARS {
            break;
        }
        quoted.push_str(&next);
    }
    if quoted.is_empty() {
        return String::new();
    }
    format!(
        "Pay close attention to the following terms when transcribing — \
         these are domain-specific names, acronyms, or vocabulary the speaker \
         is likely to use: {quoted}."
    )
}

/// Per-stream configuration options.
#[derive(Debug, Clone, Default)]
pub struct StreamOpts {
    /// Language hint (ISO 639-1, e.g., "vi"). `None` = auto LID.
    pub language_hint: Option<String>,

    /// Enable mid-sentence language identification (Soniox feature).
    pub enable_lid: bool,

    /// Enable speaker diarization. MVP 0.1: defer (false).
    pub enable_diarization: bool,

    /// Translate to this language code (e.g., "vi", "en"). `None` = transcript only.
    pub enable_translation_to: Option<String>,
}

/// Which STT engine drives the pipeline.
///
/// Selected at app startup from the persisted user config; changing it in the
/// UI updates the config but requires a restart to take effect (hot-swap of
/// the running stream is not supported yet — would need to tear down the
/// active provider task and re-spawn).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ProviderMode {
    /// Local Whisper.cpp via `WhisperLocalProvider`. Free, runs on GPU/CPU,
    /// offline. Batches per utterance — best partial granularity ~1.5 s.
    #[default]
    LocalWhisper,
    /// Soniox WebSocket via `SonioxProvider`. BYOK ($0.003/min). True
    /// word-level streaming partials, language ID, optional translation.
    SonioxCloud,
    /// OpenAI gpt-realtime via the Realtime WebSocket API. BYOK.
    /// Provider implementation pending — selecting this returns an error
    /// at startup until the provider lands.
    OpenAIRealtime,
}

impl ProviderMode {
    /// Stable string id used by config persistence + UI option labels.
    pub fn id(&self) -> &'static str {
        match self {
            Self::LocalWhisper => "local-whisper",
            Self::SonioxCloud => "soniox",
            Self::OpenAIRealtime => "openai-realtime",
        }
    }

    /// Parse from the stable id (returns `None` on unknown).
    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "local-whisper" => Some(Self::LocalWhisper),
            "soniox" => Some(Self::SonioxCloud),
            "openai-realtime" => Some(Self::OpenAIRealtime),
            _ => None,
        }
    }
}

/// What the second Whisper pass (the "Bản dịch" column) should produce.
///
/// Default is `TranslateToEnglish` — produces real English translation regardless
/// of input language. Pre-fix default `Hint("vi")` produced phonetic transliteration
/// garbage on English source ("khá tệ hại" complaint).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum TargetMode {
    /// Don't run pass B. UI just shows "Bản gốc" populated; "Bản dịch" stays
    /// in `translation_done=true` empty state ("(không có)").
    Off,
    /// Run Whisper with `set_translate(true)` — the model's only true
    /// cross-lingual translation, target language is always English.
    /// Use this when the user wants English subtitles for any source.
    #[default]
    TranslateToEnglish,
    /// Run Whisper with `set_language(Some(code))` and `set_translate(false)`.
    /// Real transcription of the source forced into the named language —
    /// gives correct text only when the input is actually that language;
    /// produces phonetic transliteration garbage on cross-lingual input.
    /// Useful when user wants a denoised/normalized version of the source
    /// (e.g., source is Vietnamese → target also Vietnamese for cleanup).
    Hint(String),
}

/// Live STT configuration that can change between utterances.
///
/// Held behind `Arc<Mutex<...>>` and read by the streaming provider on every
/// utterance — the UI can mutate this without restarting the stream.
///
/// After mutating `dictionary`, always call `rebuild_dictionary_caches()` so
/// all three provider-specific precomputed strings stay in sync.
#[derive(Debug, Clone, Default)]
pub struct LiveSttConfig {
    /// Source language for "Bản gốc" (pass A). `None` = Whisper auto-detect.
    /// When `Some("en")` the model is told upfront → faster + more accurate
    /// than relying on the auto-detect head, at the cost of producing
    /// gibberish if the input isn't actually that language.
    pub source_language: Option<String>,

    /// Behavior for "Bản dịch" (pass B). See `TargetMode` doc.
    pub target_mode: TargetMode,

    // ── Phase 8: Dictionary (custom vocabulary) ──────────────────────────────
    // Privacy: phrase strings are user content. NEVER write them to tracing.
    // Only log `dictionary.len()` or integer metrics.

    /// Ordered list of user-defined vocabulary entries (max 100).
    pub dictionary: Vec<DictEntry>,

    /// Precomputed Whisper `initial_prompt` string — rebuilt on every
    /// `rebuild_dictionary_caches()` call. Read per-utterance in the hot path.
    pub dictionary_prompt: String,

    /// Precomputed Soniox `context.terms` list — rebuilt on mutation.
    /// Consumed once at WebSocket session open; won't apply mid-session.
    pub dictionary_soniox_terms: Vec<String>,

    /// Precomputed OpenAI `session.update.instructions` string — rebuilt on
    /// mutation. Consumed once at WebSocket session open.
    pub dictionary_openai_instructions: String,
}

impl LiveSttConfig {
    /// Recompute all three provider-specific cache strings from `self.dictionary`.
    ///
    /// Must be called after any mutation of `self.dictionary`. The cost is O(N)
    /// on the entry count (N ≤ 100), taking < 1 ms — cheap compared to any I/O.
    ///
    /// Privacy: the resulting cached strings contain user phrases. Never log them.
    pub fn rebuild_dictionary_caches(&mut self) {
        self.dictionary_prompt = build_whisper_prompt(&self.dictionary);
        self.dictionary_soniox_terms = build_soniox_terms(&self.dictionary);
        self.dictionary_openai_instructions = build_openai_instructions(&self.dictionary);
    }
}

/// Convenience: handle to the shared live config.
pub type LiveConfigHandle = Arc<Mutex<LiveSttConfig>>;

/// Streaming STT provider trait.
///
/// Consumes utterances (full audio chunks bounded by VAD), emits transcript
/// events (partial + final tokens + translations) via mpsc.
///
/// Trait object compatible (uses `async_trait` macro) for runtime provider swap.
#[async_trait]
pub trait StreamingTranscriber: Send + Sync {
    /// Run the streaming transcription loop.
    ///
    /// Consumes utterances from `audio_rx` until the channel is closed,
    /// streams resulting events to `event_tx`. Blocks for the lifetime of
    /// the stream (suitable for `tokio::spawn`).
    ///
    /// # Errors
    /// - `SttError::ReconnectExhausted` if WebSocket fails persistently.
    /// - `SttError::DownstreamClosed` if `event_tx` consumer dropped.
    /// - `SttError::Provider`/`Network` for transient runtime issues.
    async fn transcribe_stream(
        &self,
        audio_rx: mpsc::Receiver<Utterance>,
        event_tx: mpsc::Sender<TranscriptEvent>,
        opts: StreamOpts,
    ) -> Result<(), SttError>;

    /// Stable provider identifier (e.g., "soniox", "openai-realtime", "google-chirp3").
    fn name(&self) -> &'static str;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(phrase: &str) -> DictEntry {
        DictEntry {
            phrase: phrase.to_string(),
            context: DictContext::Common,
        }
    }

    // ── Whisper prompt builder ────────────────────────────────────────────────

    #[test]
    fn whisper_prompt_empty_dictionary_returns_empty() {
        assert_eq!(build_whisper_prompt(&[]), "");
    }

    #[test]
    fn whisper_prompt_five_entries_comma_joined() {
        let entries = vec![
            entry("OKR"),
            entry("Vọng"),
            entry("WebSocket"),
            entry("API"),
            entry("BYOK"),
        ];
        let prompt = build_whisper_prompt(&entries);
        assert!(!prompt.is_empty());
        // Longest first — "WebSocket" (9 chars) should precede "OKR" (3 chars)
        let ws_pos = prompt.find("WebSocket").unwrap();
        let okr_pos = prompt.find("OKR").unwrap();
        assert!(ws_pos < okr_pos, "longest phrase should appear first");
        // Result must fit within token budget
        let approx_tokens: usize = prompt.len() / 3 + 5;
        assert!(approx_tokens < 210, "token estimate {approx_tokens} exceeds budget");
    }

    #[test]
    fn whisper_prompt_truncates_200_long_phrases() {
        // 200 entries with phrase length ~12 chars each — should be capped
        let entries: Vec<DictEntry> = (0..200)
            .map(|i| entry(&format!("phrase_{i:04}")))
            .collect();
        let prompt = build_whisper_prompt(&entries);
        // Byte length must stay well under 800 (200 tokens × 4 bytes/token headroom)
        assert!(
            prompt.len() <= 800,
            "prompt length {} exceeds 800 byte safety cap",
            prompt.len()
        );
    }

    #[test]
    fn whisper_prompt_no_op_when_empty() {
        let mut cfg = LiveSttConfig::default();
        cfg.rebuild_dictionary_caches();
        assert_eq!(cfg.dictionary_prompt, "");
        assert!(cfg.dictionary_soniox_terms.is_empty());
        assert_eq!(cfg.dictionary_openai_instructions, "");
    }

    // ── Soniox terms builder ──────────────────────────────────────────────────

    #[test]
    fn soniox_terms_respect_char_budget() {
        // 200 entries each 50 chars — total would be 10,000; must be capped at 8,000
        let entries: Vec<DictEntry> = (0..200)
            .map(|i| entry(&"A".repeat(50 - (i % 10))))
            .collect();
        let terms = build_soniox_terms(&entries);
        let total_chars: usize = terms.iter().map(|t| t.len() + 1).sum();
        assert!(total_chars <= 8_001, "total chars {total_chars} exceeds 8,000-char budget");
    }

    #[test]
    fn soniox_terms_empty_dictionary() {
        assert!(build_soniox_terms(&[]).is_empty());
    }

    // ── OpenAI instructions builder ───────────────────────────────────────────

    #[test]
    fn openai_instructions_empty_dictionary_returns_empty() {
        assert_eq!(build_openai_instructions(&[]), "");
    }

    #[test]
    fn openai_instructions_escapes_double_quotes() {
        let entries = vec![DictEntry {
            phrase: r#"Say "Hello""#.to_string(),
            context: DictContext::Technical,
        }];
        let instr = build_openai_instructions(&entries);
        // Should not contain an unescaped bare double-quote inside the term
        assert!(instr.contains(r#"\"Hello\""#), "quote in phrase must be escaped: {instr}");
    }

    #[test]
    fn openai_instructions_contains_prefix() {
        let entries = vec![entry("OKR"), entry("BYOK")];
        let instr = build_openai_instructions(&entries);
        assert!(
            instr.contains("Pay close attention"),
            "instructions must start with the standard prefix"
        );
    }

    // ── DictContext serialization ─────────────────────────────────────────────

    #[test]
    fn dict_context_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&DictContext::Common).unwrap(),
            "\"common\""
        );
        assert_eq!(
            serde_json::to_string(&DictContext::Names).unwrap(),
            "\"names\""
        );
        assert_eq!(
            serde_json::to_string(&DictContext::Technical).unwrap(),
            "\"technical\""
        );
    }

    #[test]
    fn dict_entry_roundtrip_json() {
        let e = DictEntry {
            phrase: "Vọng AI Recorder".to_string(),
            context: DictContext::Names,
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: DictEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }
}
