# Changelog

All notable changes to Vọng AI Recorder. Format follows [Keep a Changelog](https://keepachangelog.com/),
versioning follows [SemVer](https://semver.org/) (pre-1.0 = breaking changes possible at minor).

## [Unreleased]

Sprint 1 + 2 — production v1.0-beta.1 readiness, then power-user polish.
Sprint 1 (6 phases): MSI installer, in-app model downloader, first-run
wizard, landing page, R2 release pipeline, beta QA docs. Sprint 2 (4 phases):
Recording settings sliders, custom Dictionary, opt-in Sentry crash reporting,
LLM session summaries (OpenAI gpt-4o-mini).

### Added

- **Phase 1 — Installer** (`vong-app/wix/main.wxs`, `[package.metadata.wix]`).
  Per-user MSI via `cargo-wix` 0.3.9 + WiX Toolset v3.14.1. Installs to
  `%LocalAppData%\Programs\Vong AI Recorder\` — no UAC. Stable UpgradeCode
  GUID `FFC0DF45-51D5-45F8-A336-A5113CAFC06D` committed so future releases
  upgrade in place. Code signing cert deferred (SmartScreen warning
  documented in onboarding wizard + landing page).
- **Phase 2 — In-app Whisper model downloader** (`vong-transcribe::model_dl`).
  Streaming HTTP via `reqwest` 0.12 + `sha2` 0.10. Hash fetched live from
  HuggingFace LFS pointer (never hardcoded). Atomic `.part` → `.bin` rename
  on hash match. Cancel via `Arc<AtomicBool>` checked per chunk. Auto-cleanup
  of orphan `.part` files older than 1 hour on app startup. 4-model catalog
  (tiny / base / small / medium). EMA-smoothed ETA. 17 new tests.
- **Phase 3 — First-run wizard** (`vong-app/src/wizard.rs`,
  `vong-app/ui/wizard.slint`). 7-step overlay replacing minimal banner —
  Welcome → AudioSource → Provider → ApiKey (conditional) → ModelDownload
  (conditional) → Language → Done. Schema-versioned `onboarded.txt`
  persistence with crash-resume. Live WS-ping test for Soniox + OpenAI
  Realtime API keys (5s timeout). Mounts Phase 2 downloader at step 4.
  25 new tests.
- **Phase 4 — Landing page** (separate repo `E:\AgentAI\AI Translate\vong-landing\`).
  Polish of existing 726-line bilingual HTML draft. Adds Download section
  with SHA-256 placeholders, SmartScreen explainer, 6-screenshot showcase,
  8-FAQ, OG meta tags, robots.txt, sitemap.xml, vercel.json security
  headers. Privacy policy rendered to standalone HTML. Initial local commit;
  user deploys + configures DNS.
- **Phase 5 — Release pipeline** (`.github/workflows/release.yml`,
  `.github/r2-setup.md`, `scripts/release-checklist.md`, `scripts/release-r2-cleanup.ps1`).
  Tag-triggered 4-job pipeline: parallel CPU + Vulkan MSI builds → R2
  upload (versioned + `latest/` mirror) → draft GitHub Release. SHA-256
  computed and surfaced in `$GITHUB_STEP_SUMMARY`. Vulkan job is
  `continue-on-error` (cert/SDK issues never block the CPU release).
  Dry-run path via `workflow_dispatch` skips upload + release for first
  pipeline validation.
- **Phase 6 — Beta launch QA + announcement** (`docs/known-issues-beta.md`,
  `docs/qa-checklist-beta.md`, `docs/beta-announcement-{vi,en}.md`,
  `docs/beta-tester-recruitment.md`, `docs/feedback-channels.md`,
  `scripts/qa-smoke.ps1`). 30-case QA test matrix for clean Win11 VM. 14
  known issues catalogued (0 blockers, 6 warnings, 8 info). Vietnamese
  announcement draft (~500 words, hashtag-ready). English short version
  (~180 words) for international dev channels. Feedback channel
  recommendation: Google Form (primary) + Telegram (~10 power testers) +
  GitHub Discussions tertiary. PowerShell smoke script with 8 automated
  checks for post-install validation.
- **Phase 7 — Recording settings: VAD sliders + Whisper model picker**
  (`vong-app/src/recording_config.rs`, `vong-audio::LiveVadConfig`,
  `WhisperLocalProvider::swap_context`). Settings -> Recording tab fully
  wired: 3 sliders (threshold 0.30-0.95, hangover 100-800ms, max_duration
  3-15s) live-apply via `Arc<Mutex<VadConfig>>` snapshot pattern - next
  VAD tick picks them up, no restart. Model picker dropdown scans models
  dir; Switch button triggers background `WhisperContext` re-load with
  loading overlay. Persists to
  `%APPDATA%\Vong\Vong AI Recorder\config\recording.json` (atomic write). 12 new tests.
- **Phase 8 — Dictionary (custom vocabulary)** (`vong-app/src/dictionary.rs`,
  `vong-transcribe::traits::{DictEntry, DictContext}` +
  `build_{whisper_prompt,soniox_terms,openai_instructions}` helpers).
  Settings -> Dictionary tab: up to 100 entries (phrase + context category:
  Common / Names / Technical) with add / edit / move / delete. Provider-
  specific injection: Whisper `initial_prompt` (longest phrases first,
  ~200 tokens), Soniox `context.terms` array, OpenAI Realtime
  `session.update.instructions`. Persists to
  `%APPDATA%\Vong\Vong AI Recorder\config\dictionary.json`. Whisper picks up
  live; Soniox / OpenAI require next stream start (UI hint). Privacy: phrase
  content never logged - only entry counts. 29 new tests.
- **Phase 9 — Crash reporting opt-in (Sentry)** (`vong-app/src/sentry_init.rs`,
  `vong-app/src/wizard.rs` extended). Sentry 0.34 + sentry-tracing bridged
  into existing tracing subscriber. Default OFF - explicit consent: new
  wizard step `CrashReport` between Language and Done (Sprint 1 alpha users
  who already completed wizard stay complete) + Settings -> System toggle.
  `before_send` scrubber: drops events whose fields contain transcript /
  audio / phrase / dictionary_entry / api_key / password / model_path,
  truncates message bodies to 200 chars, IP stripped. Hardcoded DSN
  placeholder `REPLACE-ME-WITH-VONG-OWNED-DSN` - user must create Sentry
  project + paste real DSN pre-tag. Consent persists to
  `%APPDATA%\Vong\Vong AI Recorder\config\sentry.txt`. 30 new tests.
- **Phase 10 — Session summaries (LLM end-of-session)**
  (`vong-app/src/{autosummary,summary_runner}.rs`,
  `vong-transcribe/src/summary.rs`). On Stop / app close, spawn background
  task that fetches transcript -> builds Vietnamese prompt -> calls OpenAI
  `gpt-4o-mini` chat completions reusing the `openai-realtime` keychain key
  -> persists to existing `summaries` table (Phase 6 schema). Retry once on
  429/5xx (honors `retry-after`); regenerate capped at 3x. Settings -> System
  toggle (default ON when key exists). Cost ~$0.0006/session for 30-min
  meeting. HistoryCard preview + SessionDetail summary card deferred to
  Sprint 3 (HistoryCard needs VecModel refactor). 14 new tests.

### Changed

- **Workspace version**: `0.1.0-alpha.1` → `0.1.0-beta.1`. Cascades to all
  5 crates via `version.workspace = true`.
- **Slint UI**: `OnboardingBanner` component removed in favor of full
  wizard overlay. `current-view` now routes `wizard` | `main` | `settings`.
- **`vong-transcribe::traits`**: `ProviderMode` + `TargetMode` now use
  `#[derive(Default)]` with `#[default]` markers (fixes `derivable_impls`
  clippy warning that was blocking CI gate).
- **`vong-transcribe` whisper-rs declaration**: split into target-specific
  Cargo dependency blocks. macOS builds now auto-enable the `metal` feature
  (whisper.cpp Metal GPU backend) — no `--features` flag needed, no SDK
  install beyond `brew install cmake`. Windows/Linux unchanged: CPU by
  default, `--features vulkan` still opt-in. CoreML (Apple Neural Engine)
  deliberately deferred.
- **`vong-audio`**: `VadConfig` now derives `Serialize` + `Deserialize`
  (excluding `partial_emit_ms` which stays a protocol constant). New
  `LiveVadConfig = Arc<Mutex<VadConfig>>` type alias + `run_vad_fsm_live()`
  alongside the original. FSM snapshots from the handle at the top of each
  process tick.
- **`WhisperLocalProvider`**: `ctx` and `model_label` wrapped in
  `Mutex<Arc<...>>` to support atomic `swap_context()` for live model
  switching without restarting the streaming task.
- **`vong-transcribe::traits::LiveSttConfig`**: 4 new fields wiring the
  dictionary into per-utterance snapshot - `dictionary: Vec<DictEntry>`,
  `dictionary_prompt: String` (Whisper cache), `dictionary_soniox_terms:
  Vec<String>` (Soniox cache), `dictionary_openai_instructions: String`
  (OpenAI cache). `rebuild_dictionary_caches()` regenerates the 3 caches
  from the canonical `dictionary` field.
- **`vong-storage`**: `NewSummary` + `Summary` structs + `insert_summary` /
  `fetch_latest_summary` / `delete_summaries_for_session` repository helpers.
  Schema from Phase 6 (`summaries` table) unchanged.
- **Wizard schema**: still `v=2`, but step count grew from 7 to 8
  (`CrashReport` inserted at index 6, `Done` shifted to 7). Sprint 1 alpha
  users who already have `step.complete=...` in `onboarded.txt` are NOT
  re-prompted (legacy completion is honored).
- **`deny.toml`**: 4 RUSTSEC advisory ignores added for `rustls-webpki
  0.102.8` CVEs (RUSTSEC-2026-0049/0098/0099/0104) - transitive via
  `sentry 0.34 -> rustls 0.22.4`. Low risk: outbound Sentry transport only,
  not user data path. Remove when `sentry-rust` bumps the rustls-webpki dep.

### Fixed

- Pre-existing clippy warnings that would have blocked CI `-D warnings`
  gate after Phase 2 landed (`&PathBuf` → `&Path`, doc list item
  indentation in `default_stream_opts`).

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
