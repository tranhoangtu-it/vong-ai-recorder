# Vọng AI Recorder

> Cuộc họp đa ngôn ngữ. Không một câu bị bỏ sót.

Desktop transcription app cho Windows 10/11 với real-time Vietnamese-first transcription, mic + speakers (loopback) capture, Whisper Base on-device (CPU/GPU), FTS5 tone-insensitive search. **Audio không bao giờ rời máy.**

**Status**: 🟢 Phase 0-7 wired (Alpha). MVP 0.1 production-ready candidate.
**Primary platform**: Windows 10/11. macOS scaffolding compiles but features paused.

## Tính năng đã hoạt động

- 🎙 **Capture** — mic hoặc speakers (WASAPI loopback) qua cpal + MMCSS RT priority
- 🔁 **Hot-swap source** — đổi mic/speakers trong khi đang ghi, không cần restart
- 🎯 **VAD** — earshot phát hiện giọng nói, đóng gói utterance tự động
- 🧠 **Whisper Base local** — CPU (~12-20s/utterance) hoặc Vulkan GPU trên Windows (~0.15-1.2s, 35-100× faster) hoặc Metal GPU trên macOS (auto, không cần SDK)
- 📝 **SQLite + FTS5** — sessions + segments persist, NFC-normalize, tone-insensitive search ("viet" khớp "Việt")
- 🔥 **GPU warmup** — pre-pay shader-pipeline init ở startup, user không thấy lag mid-utterance
- 🪟 **System tray + global hotkey** — Ctrl+Shift+R toggle window visibility
- 💊 **Floating Pill** — borderless overlay 360×140 always-on-top, live transcript + peak meter
- 📥 **Export Markdown** — xuất phiên ghi ra `~/Documents/Vong/session-N.md`
- 👋 **First-launch wizard** — 8-step flow (Welcome -> AudioSource -> Provider -> ApiKey -> Model -> Language -> CrashReport -> Done), crash-resume, live API-key test
- 🎛 **Recording settings sliders** — VAD threshold / hangover / max-duration tune live (no restart) + Whisper model picker switches `ggml-{tiny,base,small,medium}.bin` atomically
- 📚 **Dictionary** — tới 100 từ vựng riêng (tên người, thuật ngữ chuyên ngành) inject vào Whisper `initial_prompt` / Soniox `context` / OpenAI Realtime `instructions`
- 🛡 **Crash reporting opt-in (Sentry)** — gửi crash report ẩn danh, mặc định TẮT, scrubber bóc transcript/audio/API key/file path khỏi events
- ✨ **AI session summaries** — sau khi dừng ghi, OpenAI `gpt-4o-mini` tóm tắt tiếng Việt 3-5 câu (~$0.0006/phiên 30 phút)
- 📜 **Log rotation** — daily rolling file ở `%APPDATA%\Vong\Vong AI Recorder\data\logs\`

## Quick Start (Dev)

### Prerequisites
- Rust 1.78+ (`rustup install stable`)
- **Windows 10 / 11** — primary dev target (this pivot is temporary; macOS-first
  resumes after Phase 5 UI ships on Windows)
- macOS 13.0+: scaffolding still compiles. Phase 2 macOS loopback (screencapturekit),
  Phase 5 macOS tray/vibrancy, Phase 7 macOS permission probing → paused until Windows
  MVP 0.1 ships. Code stays behind `#[cfg(target_os = "macos")]` so it doesn't break the Windows build.

### Build

```powershell
cd app
cargo build --release                          # Default: CPU-only Whisper (~8 MB binary)
.\target\release\vong.exe                      # Windows (primary)
# macOS/Linux: ./target/release/vong  (scaffolding only — features deferred)
```

#### GPU build options

##### Windows — Vulkan opt-in (NVIDIA / AMD / Intel)

```powershell
# One-time: install Vulkan SDK (~750 MB, includes headers + libs)
winget install KhronosGroup.VulkanSDK

# Per-shell: set toolchain env (Ninja avoids VS 2026 MSBuild `vulkan-shaders-gen` bug)
$env:VULKAN_SDK = "C:\VulkanSDK\1.4.350.0"     # use the version winget installed
$env:CMAKE_GENERATOR = "Ninja"

cargo build --release --features vulkan        # ~60 MB binary (SPIR-V shaders embedded)
```

##### macOS — Metal auto (Apple Silicon M-series)

```bash
# One-time: just cmake. Metal SDK ships with macOS — no extra install.
brew install cmake

# Metal backend is auto-enabled via target-specific dependency in
# vong-transcribe/Cargo.toml. No --features flag needed.
cargo build --release --bin vong
```

CoreML (Apple Neural Engine) is deliberately NOT auto-enabled — adds
~30s first-run ANE compile + separate model file. Deferred to a future
sprint.

Verified end-to-end on Windows 11:
- Phase 0: Slint window "Vọng" opens, panic-safe logging initialized
- Phase 1: cpal WASAPI mic capture, MMCSS RT-priority promotion, live peak meter in UI
- Phase 3: earshot VAD FSM packing utterances
- Phase 4 (local CPU): Whisper.cpp via `whisper-rs`, base multilingual, ~12-20 s per 5 s utterance
- Phase 4 (local GPU Vulkan): RTX A2000 inference **0.15-1.2 s per 0.6-5.7 s utterance** — 35-100× faster than CPU realtime, 5× higher throughput
- Phase 5: tray icon (Show / Hide / Quit) + Ctrl+Shift+R hotkey + Floating Pill 360×140
- Phase 6: SQLite FTS5 (`unicode61 remove_diacritics 2`) — sessions + segments persisted, History card visible in dashboard

### Local STT setup (Phase 4 — no cloud key required)

The default Whisper Base model (multilingual, 142 MB) is searched in this order:

1. Path in env var `VONG_WHISPER_MODEL` (explicit override)
2. `<exe_dir>/models/ggml-base.bin` next to the binary
3. `%LOCALAPPDATA%\Vong\Vong\data\models\ggml-base.bin`

Download once:

```powershell
$dir = "target\debug\models"           # or release/models for release build
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Invoke-WebRequest `
    -Uri "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin" `
    -OutFile "$dir\ggml-base.bin"
```

First `cargo build` after adding `whisper-rs` compiles `whisper.cpp` from C source —
**3-5 minutes** on Windows (needs `cmake` + MSVC build tools). Subsequent builds incremental.

Larger / smaller models (drop in same dir, change filename via env var):

| Model | Size | RAM (peak) | CPU latency / 30 s audio | VN quality |
|---|---|---|---|---|
| `ggml-tiny.bin` | 75 MB | 110 MB | ~1 s | weak |
| `ggml-base.bin` | **142 MB** | 210 MB | ~2-3 s | **default — acceptable** |
| `ggml-small.bin` | 466 MB | 600 MB | ~5-8 s | good |
| `ggml-medium.bin` | 1.5 GB | 2 GB | ~20 s (CPU) / real-time (GPU) | excellent |

### CI Gates (local)

```bash
cargo fmt --all -- --check         # Format
cargo clippy --workspace -- -D warnings  # Lint
cargo audit                         # CVE check
cargo deny check                    # License + policy
cargo test --workspace              # Unit tests
```

## Architecture

```
vong-app/      Main binary — Slint UI, tray + global hotkey, pipeline orchestration
vong-audio/    Audio capture + resampler + VAD (Phase 1-3)
               cpal WASAPI (input + loopback) → rtrb SPSC ring → rubato sinc resampler
               → earshot VAD FSM → Utterance pack. 16 kHz mono i16 target.
vong-transcribe/ Transcription providers (3 backends behind one trait)
               · WhisperLocalProvider (whisper.cpp via whisper-rs, CPU / Vulkan / macOS Metal)
               · SonioxProvider (WebSocket BYOK — word-level partials)
               · OpenAIRealtimeProvider (WebSocket BYOK — gpt-4o-mini-transcribe)
vong-storage/  SQLite + FTS5 (`unicode61 remove_diacritics 2`) — sessions + segments,
               NFC normalize, search, Markdown/SRT export
vong-ui/       Reusable Slint components (Phase 5-future — currently inlined in vong-app)
```

## Roadmap status

| Phase | Status | Description |
|---|---|---|
| 0 | ✅ Complete | Scaffolding + tooling + CI gates |
| 1 | ✅ Wired | Mic capture (cpal WASAPI + MMCSS RT + U8/I16/U16/F32 formats) |
| 2-W | ✅ Wired | Windows loopback (output devices via WASAPI loopback flag) |
| 3 | ✅ Wired | earshot VAD FSM + utterance packaging |
| 4 | ✅ Wired | Whisper Base local (CPU + Vulkan opt-in + macOS Metal auto) + warmup |
| 5 | ✅ Wired | Tray + global hotkey + Floating Pill |
| 6 | ✅ Wired | SQLite FTS5 + History UI + Search UI + Markdown export |
| 7 | ✅ Minimal | First-launch onboarding banner + dismiss persist |
| 7 | ✅ Wired | First-run wizard (8 steps) + onboarding state machine |
| 8 | ✅ Wired | MSI installer (per-user, unsigned) - needs WiX Toolset v3.14.1 to build locally |
| 9 | ✅ Wired | R2 release pipeline (CI tag-trigger -> MSI build -> upload -> draft GH Release) |
| 10 | ✅ Wired | Recording settings sliders + Whisper model picker (live-apply) |
| 11 | ✅ Wired | Dictionary (custom vocabulary, <=100 entries, 3-provider injection) |
| 12 | ✅ Wired | Crash reporting opt-in (Sentry, default OFF, scrubbed) |
| 13 | ✅ Wired | AI session summaries (OpenAI gpt-4o-mini, end-of-session) |
| 2-macOS | ⏸ Paused | screencapturekit + clock drift — deferred until Windows ships |

**Wiring status**: All 7 wired phases functional via `vong-app/src/main.rs` pipeline.

### Phase 8 — MSI installer (TODO)

Distribute as Windows MSI via `cargo-wix`:

```powershell
# One-time setup
cargo install cargo-wix
winget install WiXToolset.WiX            # ~30 MB

# Inside `app/`:
cargo wix init                            # generates wix/main.wxs template
# Edit wix/main.wxs: product name, manufacturer, upgrade GUID, icon
cargo wix --no-build --features vulkan    # if shipping GPU build
# Outputs: target/wix/Vọng-0.1.0-x86_64.msi

# Code signing (production — needs Authenticode cert)
signtool sign /a /tr http://timestamp.digicert.com /td sha256 /fd sha256 `
    target\wix\Vọng-0.1.0-x86_64.msi
```

Without signing, Windows SmartScreen shows "Unknown publisher" warning on first install
(user must click "More info" → "Run anyway"). Acceptable for alpha; production needs
Sectigo OV (~$200/yr) or DigiCert EV (~$500/yr) Authenticode certificate.

## Privacy & Security

- Audio NEVER leaves your machine to Vọng servers (BYOK direct to provider)
- API keys stored in OS-native credential store via `keyring` crate
  (Windows Credential Manager on Windows; macOS Keychain when macOS resumes)
- Logs sanitized (no audio content, no API keys, no user data)
- Tuân thủ Luật Bảo vệ Dữ liệu Cá nhân Việt Nam 91/2025/QH15
- See [SECURITY.md](./SECURITY.md) for vulnerability disclosure

## License

AGPL-3.0-only cho OSS core. Pro tier (future) sẽ proprietary.

⚠️ Slint dual-licensed (GPL-3.0 / Commercial). Sử dụng Slint GPL-3.0 cho free tier. Pro tier require Slint Royalty-free Commercial license — pending decision (plan v2 blocker #1).

## Documentation

In-repo:
- [Development guide](./docs/development.md)
- [Security policy](./SECURITY.md)
- [AI agent guide (CLAUDE.md)](../CLAUDE.md) — workspace layout, invariants, code-review fixes

External (developer-local, not in this repo):
- Implementation plan + 9 phase files — `~/.claude/plans/260517-1500-vong-ai-recorder/`
- Design doc v2 (2,046 lines, 7 review passes) — `~/.claude/plans/t-p-trung-v-nghi-n-greedy-pixel.md`
- Original source spec — `../Thiết kế ứng dụng ghi âm STT đa nền tảng.md` (parent of this dir)
- Privacy Policy bilingual draft (PDPL 2026, pending lawyer review) — `~/.claude/plans/vong-privacy-policy.md`

## Contact

- Email: `dev@vong.app`
- Security: `security@vong.app`
- Privacy: `privacy@vong.app`
