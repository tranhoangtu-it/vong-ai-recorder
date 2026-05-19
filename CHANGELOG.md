# Changelog

All notable changes to Vọng AI Recorder. Format follows [Keep a Changelog](https://keepachangelog.com/),
versioning follows [SemVer](https://semver.org/) (pre-1.0 = breaking changes possible at minor).

## [Unreleased]

## [0.1.0-alpha.1] — 2026-05-17

First Windows alpha. End-to-end Vietnamese-first transcription pipeline running
locally with zero cloud dependency.

### Added

- **Phase 1** — cpal WASAPI mic capture with MMCSS RT-priority promotion.
  Sample formats: F32, I16, U16, U8 (Windows default Realtek mic).
- **Phase 2-W** — Windows loopback capture from output devices via WASAPI
  loopback flag. `list_all_devices()` enumerates inputs + outputs. Hot-swap
  between sources without restart — `AudioSource` in `Rc<RefCell<>>`,
  shared peak/overflow/audio_tx keeps VAD/Whisper downstream connected.
- **Phase 3** — earshot VAD FSM (threshold 0.5, pre-roll 200 ms, hangover
  400 ms) packaging utterances.
- **Phase 4** — Whisper Base local STT via `whisper-rs` 0.16.
  - CPU-only by default (~8 MB binary, ~12-20 s per 5 s utterance)
  - Opt-in Vulkan GPU via `--features vulkan` (~60 MB binary,
    ~0.15-1.2 s per utterance, 35-100× faster than CPU realtime on RTX A2000)
  - Warmup task on blocking pool pre-pays first-call shader-pipeline init
    (~6 s cold, ~1.3 s driver-cached).
- **Phase 5** — System tray icon (16×16 violet, generated in-process) with
  Show / Hide / Quit menu. Ctrl+Shift+R global hotkey toggles window
  visibility. Floating Pill 360×140 borderless always-on-top with live
  transcript + peak meter + timer + utterance count + close button.
- **Phase 6** — SQLite storage with FTS5 `unicode61 remove_diacritics 2`
  Vietnamese tone-insensitive search. Sessions + segments persisted with
  NFC normalize. History card refreshed at ~1 Hz, FTS5 search-as-you-type,
  Markdown export to `~/Documents/Vong/session-N.md`.
- **Phase 7** — First-launch onboarding banner with 3-feature welcome.
  Dismiss persists to `%APPDATA%\Vong\Vong AI Recorder\config\onboarded.txt`.

### Production polish

- Daily-rolling crash log file at `%APPDATA%\Vong\Vong AI Recorder\data\logs\` via
  `tracing-appender` 0.2. ANSI-stripped output, panic-safe, payload elided
  for privacy.
- `tracing` metadata-only (no audio bytes, transcripts, API keys, or PII).
- `keyring` 3.6 (Windows Credential Manager) for future API key storage.
- `secrecy` + `zeroize` for in-memory API key protection.

### Fixed

- `rubato` 0.16 `process_into_buffer` output Vec sizing — pre-size to
  `output_frames_max()` then iterate `[..output_frames_next()]` valid prefix
  (the previous `Vec::with_capacity` left len=0 → "Insufficient buffer size 0"
  at runtime).
- `cpal` 0.17 `negotiate_config` kind-aware — output devices use
  `supported_output_configs` + `default_output_config` (else cpal errors
  "stream type not supported" on loopback).
- Whisper empty text (silence/low-confidence) no longer clobbers
  `last-transcript` UI — UI keeps the last meaningful line.
- VAD utterance start log mislabeled `duration_ms` as `seq` — fixed to
  emit `seq=onset_seq, initial_ms=duration_ms`.

### Known limitations

- FTS5 `unicode61 remove_diacritics 2` does not normalize Vietnamese
  `đ` (U+0111) — `duoc` does NOT match `được`. Workaround: query with
  `đuoc` or pre-normalize `đ → d` in `models::normalize` (future).
- MSI installer build setup documented but not yet executed (requires
  `cargo install cargo-wix` + `choco install wixtoolset`).
- MSI is unsigned in alpha — Windows SmartScreen warns on first install
  ("Unknown publisher"). Production signing requires Sectigo OV
  (~$200/yr) or DigiCert EV (~$500/yr) Authenticode certificate.
- macOS feature work paused — `#[cfg(target_os = "macos")]` gates stay
  green via cross-compile sanity check but `screencapturekit` loopback
  + permission probing deferred until Windows MVP 0.1 ships.

[Unreleased]: https://github.com/tranhoangtu-it/vong-ai-recorder/compare/v0.1.0-alpha.1...HEAD
[0.1.0-alpha.1]: https://github.com/tranhoangtu-it/vong-ai-recorder/releases/tag/v0.1.0-alpha.1
