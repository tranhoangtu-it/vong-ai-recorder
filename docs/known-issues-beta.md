# Known Issues — Vọng AI Recorder v0.1.0-beta.1

Last updated: 2026-05-20

This document lists limitations and known behaviours that beta testers should be
aware of before filing bug reports. Items here are **not bugs** — they are
deliberate trade-offs or deferred work for Sprint 2. If you hit something not
listed here, please report it via the feedback channel.

---

## Installer

### SmartScreen "Unknown publisher" warning

| | |
|---|---|
| **Severity** | Info |
| **Phase** | P1 (installer foundation) |
| **Workaround** | Click "More info" → "Run anyway". This is expected for unsigned binaries. |
| **SHA-256 verify** | Compare the MSI hash against the value published at https://vong.app/ before clicking through. |
| **ETA** | Code-signing certificate (Sectigo OV ~$200/yr or DigiCert EV ~$500/yr) deferred to post-beta based on adoption signal. If ≥50 active users sustain after beta: invest in cert in Sprint 2. |

### Corporate antivirus may quarantine `vong.exe`

| | |
|---|---|
| **Severity** | Warning |
| **Phase** | P1 |
| **Workaround** | Add `%LocalAppData%\Programs\Vong AI Recorder\` to your AV allowlist. Common offenders: CrowdStrike Falcon, SentinelOne, Microsoft Defender (Business). Personal Defender typically does not block after SmartScreen click-through. |
| **ETA** | Resolves with code-signing cert (same sprint as SmartScreen fix). |

### No auto-update

| | |
|---|---|
| **Severity** | Info |
| **Phase** | P1 |
| **Workaround** | Download and install the new MSI manually from https://vong.app/ when a new version is announced. Per-user install means no UAC prompt on update. |
| **ETA** | MSIX-based MSIX auto-update or Squirrel.Windows deferred to Sprint 3. |

---

## Audio Capture

### WASAPI loopback may fail if another app holds exclusive mode

| | |
|---|---|
| **Severity** | Warning |
| **Phase** | P1 (cpal/WASAPI) |
| **Workaround** | Close the other app (e.g. exclusive-mode DAW, some USB audio interfaces' companion software) and restart Vọng. |
| **ETA** | No fix planned — this is a Windows WASAPI constraint, not a Vọng bug. |

### Bluetooth audio device drift

| | |
|---|---|
| **Severity** | Info |
| **Phase** | P1 (cpal capture) |
| **Workaround** | Use a wired microphone or the built-in laptop mic for best streaming-partial timing. Bluetooth headsets introduce 100-300 ms extra latency, which shifts partial-text appearance slightly. |
| **ETA** | No fix planned — inherent Bluetooth A2DP/HFP latency. |

### macOS not supported in beta

| | |
|---|---|
| **Severity** | Info |
| **Phase** | P1 (Windows-first pivot, 2026-05-17) |
| **Workaround** | None — Windows 10/11 64-bit only for v0.1.0-beta.1. |
| **ETA** | macOS port (screencapturekit loopback + permission probing) deferred to a future sprint after Windows beta stabilises. |

---

## Transcription

### Whisper local: Vulkan cold-start ~6 seconds on first utterance

| | |
|---|---|
| **Severity** | Info |
| **Phase** | P4 (Whisper local, Vulkan GPU) |
| **Workaround** | The app pre-warms the Vulkan shader pipeline at startup. The ~6 s delay happens on first launch or after a reboot. Subsequent utterances in the same session are 0.15-1.2 s. CPU builds (default) do not have this delay. |
| **ETA** | No change planned — this is a one-time SPIR-V compilation cost inherent to the Vulkan driver. |

### Provider hot-swap requires restart

| | |
|---|---|
| **Severity** | Warning |
| **Phase** | P2 (wizard) + P3 (settings) |
| **Workaround** | After changing the STT provider in Settings → System, a banner appears: "Khởi động lại Vọng AI Recorder để áp dụng". Close and reopen the app. |
| **ETA** | Hot-swap (reconfiguring the active WebSocket session without restart) is Sprint 2 work. |

### Soniox and OpenAI Realtime: language change not applied mid-session

| | |
|---|---|
| **Severity** | Warning |
| **Phase** | P2/P3 (settings + language picker) |
| **Workaround** | Changing the source language in Settings while a Soniox or OpenAI session is active does not take effect until the next app restart. Whisper Local applies language changes immediately on the next utterance without restart. |
| **ETA** | Soniox/OpenAI live language change requires closing and reopening the WebSocket session — deferred to Sprint 2. |

### "Hint" target mode produces phonetic garbage cross-language

| | |
|---|---|
| **Severity** | Info |
| **Phase** | P4 (Whisper local, TargetMode) |
| **Workaround** | Use "Dịch sang tiếng Anh" (Translate to English) for cross-lingual transcription. Use "Tắt" (Off) to skip translation entirely. Only use "Hint" when the source and target language are the same (e.g. Vietnamese → Vietnamese cleanup). |
| **ETA** | By design — this is the expected Whisper behaviour when forced into a mismatched language. |

### No speaker diarization

| | |
|---|---|
| **Severity** | Info |
| **Phase** | Deferred beyond Sprint 1 |
| **Workaround** | All transcript text appears in a single column without per-speaker labels. For multi-speaker meetings, note manually who is speaking. |
| **ETA** | Diarization (pyannote.audio or similar) is Sprint 3+ after core pipeline stabilises. |

---

## Search & Storage

### Vietnamese search: `đ` is not matched by tone-stripping

| | |
|---|---|
| **Severity** | Warning |
| **Phase** | P3 (vong-storage FTS5) |
| **Detail** | The FTS5 `unicode61 remove_diacritics 2` tokenizer strips tone marks (e.g. `không` → `khong`) so queries like `khong` match `không`. However, `đ` (U+0111) is a **separate letter** in the Vietnamese alphabet, not a diacritic. Therefore `duoc` does **NOT** match `được`. |
| **Workaround** | Query with the actual `đ` character: type `đuoc` to find `được`. On Windows, press `đ` with a Vietnamese IME (Unikey, EVKey) or use Telex/VNI input. |
| **ETA** | Pre-normalising `đ → d` before FTS5 insert is planned for Sprint 2 storage migration. |

---

## User Interface

### Voice Typing, Dictionary, Notification tabs are placeholders

| | |
|---|---|
| **Severity** | Info |
| **Phase** | P3 (Settings screen) |
| **Workaround** | These tabs appear in Settings but contain no functional controls yet. Only System, Recording, and Language tabs have active content. |
| **ETA** | Voice Typing (hotword / push-to-talk) — Sprint 2. Dictionary (custom vocab) — Sprint 3. Notification — Sprint 3. |

### Transcript history refreshes at ~1 Hz

| | |
|---|---|
| **Severity** | Info |
| **Phase** | P3 (History card) |
| **Detail** | The History sidebar polls the database at approximately 1 Hz. A newly completed session may take up to 1 second to appear. |
| **Workaround** | Wait 1-2 seconds after stopping a recording before searching. |
| **ETA** | Event-driven History refresh (database trigger → UI push) is Sprint 2. |

---

## Summary Table

| # | Issue | Severity | Phase | ETA |
|---|---|---|---|---|
| I-01 | SmartScreen unsigned warning | Info | P1 | Post-beta (cert) |
| I-02 | Corporate AV quarantine | Warning | P1 | Post-beta (cert) |
| I-03 | No auto-update | Info | P1 | Sprint 3 |
| I-04 | WASAPI exclusive-mode conflict | Warning | P1 | Won't fix (OS constraint) |
| I-05 | Bluetooth audio drift | Info | P1 | Won't fix (BT constraint) |
| I-06 | macOS not supported | Info | P1 | Future sprint |
| I-07 | Vulkan cold-start ~6 s | Info | P4 | Won't fix (driver cost) |
| I-08 | Provider hot-swap requires restart | Warning | P2/P3 | Sprint 2 |
| I-09 | Soniox/OpenAI language change needs restart | Warning | P2/P3 | Sprint 2 |
| I-10 | "Hint" mode phonetic garbage cross-language | Info | P4 | By design |
| I-11 | No speaker diarization | Info | Deferred | Sprint 3+ |
| I-12 | `đ` not matched by FTS5 tone-strip | Warning | P3 | Sprint 2 |
| I-13 | Settings tabs: Voice Typing/Dict/Notif placeholder | Info | P3 | Sprint 2-3 |
| I-14 | History refresh ~1 Hz latency | Info | P3 | Sprint 2 |

---

*For real bugs (crashes, data loss, silent transcription failure, DB corruption), please report via the beta feedback channel linked from https://vong.app/.*
