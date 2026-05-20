# Beta Announcement — English (Short Version)

Use for: Reddit (r/vietnam, r/selfhosted, r/rust), dev.to, Hacker News (Show HN),
personal English-language blog. Target length: 150-200 words.

---

## Reddit / dev.to Post

**Vọng AI Recorder — offline Windows transcription app, built in Vietnam, open beta**

Built in Vietnam for Vietnamese-first transcription — but works for any language
Whisper supports.

**The problem**: multilingual remote meetings (Vietnamese ↔ English) with no good
offline transcription option. Existing tools send audio to the cloud; that is a
non-starter for sensitive calls.

**What Vọng does**:
- Records mic or system audio (WASAPI loopback) on Windows 10/11
- Streams partial transcripts as you speak (~1.5 s cadence via VAD)
- Auto-translates to English in a side-by-side two-column UI
- Stores everything in local SQLite with Vietnamese tone-insensitive FTS5 search
- Ships a 12 MB CPU binary (Whisper base, fully offline) — optional Vulkan GPU build for 35-100× faster inference

BYOK cloud options (Soniox, OpenAI Realtime) available if you need lower latency.
API keys stored in Windows Credential Manager, never in plaintext.

**Stack**: Rust + Slint UI + whisper-rs + cpal WASAPI + SQLite FTS5.
**License**: AGPL-3.0.

Looking for 20-30 beta testers — especially Vietnamese speakers on Windows who
attend multilingual calls regularly. Unsigned installer (SmartScreen warning
expected); SHA-256 published on the site.

Download + known issues: https://vong.app/
Source (private during beta): github.com/tranhoangtu-it/vong-ai-recorder
Beta sign-up form: [FEEDBACK_FORM_URL]

---

## Show HN Title (if posting to Hacker News)

```
Show HN: Vọng – offline Windows transcription app (Rust + Whisper + Slint, AGPL)
```

## dev.to Tags

```
#rust #windows #opensource #machinelearning
```

## Notes

- Replace `[FEEDBACK_FORM_URL]` with the actual Google Form short URL before posting.
- For r/selfhosted and r/rust: lead with the technical stack paragraph — that
  audience cares about the implementation more than the use case.
- For r/vietnam: lead with the problem statement — that community relates to the
  multilingual meeting pain directly.
- Keep responses to comments factual and specific. Avoid superlatives.
