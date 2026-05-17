# Vọng Development Guide

## Setup

### Prerequisites
- **Rust 1.78+** — `rustup install stable && rustup default 1.78`
- **macOS 13.0+** — Primary dev/test target (Apple Silicon hoặc Intel)
- **Windows** — Cross-compile only, NOT for runtime testing

### Recommended tools
```bash
cargo install cargo-audit       # CVE scanning
cargo install cargo-deny        # License + policy enforcement
cargo install cargo-watch       # Auto-rebuild on file change
cargo install cargo-flamegraph  # Performance profiling
```

## Common Commands

### Build
```bash
# Dev build (fast)
cargo build --workspace

# Release build (size-optimized per plan v2 Section 4.5)
cargo build --workspace --release

# Performance build (speed-optimized)
cargo build --workspace --profile release-perf
```

### Run
```bash
# Dev
cargo run --bin vong

# Release
./target/release/vong

# With debug logs
VONG_LOG=debug cargo run --bin vong
```

### Test
```bash
cargo test --workspace
cargo test -p vong-audio        # specific crate
cargo test capture -- --nocapture  # show stdout
```

### Lint
```bash
cargo fmt --all                          # auto-format
cargo fmt --all -- --check               # verify only
cargo clippy --workspace -- -D warnings  # lint với fail on warnings
```

### Security
```bash
cargo audit                # CVE check (RustSec advisory DB)
cargo deny check           # license + policy violations
cargo outdated             # find stale deps (informational)
```

## Workspace Structure

```
app/
├── Cargo.toml              Workspace root, profile, shared deps
├── rust-toolchain.toml     Pin Rust 1.78
├── deny.toml               Supply chain policy
├── .github/workflows/      CI
├── docs/                   This doc + future
├── vong-app/               Main binary (Slint UI entry)
├── vong-audio/             Phase 1-3 audio pipeline
├── vong-stt/               Phase 4+ STT providers
├── vong-storage/           Phase 6 SQLite + FTS5
└── vong-ui/                Phase 5+7 tray + pill + dashboard
```

## Logging

Use `tracing` macros với metadata only — never content:

```rust
// ✅ GOOD — metadata
tracing::info!(
    seq = utt.seq,
    duration_ms = utt.duration_ms,
    language = utt.language.as_deref().unwrap_or("?"),
    "VAD: utterance pack"
);

// ❌ BAD — content leak
tracing::info!("Transcribed: {}", text);          // leaks transcript
tracing::error!("Soniox error key={}", api_key);  // leaks API key
tracing::debug!("Audio: {:?}", pcm_samples);      // leaks audio
```

Enable verbose: `VONG_LOG=vong=debug cargo run`

## Phase Workflow

1. Read phase file (`plans/260517-1500-vong-stt-mvp01/phase-XX-*.md`)
2. Update task status: `ck plan check <phase-id> --start`
3. Implement per Implementation Steps section
4. Run tests + lint
5. Verify Success Criteria
6. Update task: `ck plan check <phase-id>`
7. Commit with conventional commit message

## Conventional Commits

```
feat(audio): add cpal mic capture với RT thread promotion
fix(stt): handle Soniox WebSocket reconnect timeout
docs(security): expand threat model section
chore(ci): add cargo-deny step
refactor(ui): extract tray icon state machine
test(audio): add VAD threshold edge cases
```

## Cross-Platform Notes

### macOS-specific code
Use `#[cfg(target_os = "macos")]` cho:
- screencapturekit (Phase 2)
- core-foundation, objc2 (Phase 7 permission probing)
- window-vibrancy macOS variants (Phase 5)

### Windows
Phase 8+ target. Currently cross-compile only — `cargo build` works on Windows
nhưng audio/UI features không runtime.

## Debugging

### Audio glitches
1. Verify RT thread priority — check `audio_thread_priority` return value
2. Check ring buffer overflow events trong log
3. Profile callback timing với `tracing::instrument`
4. Use `cargo flamegraph` cho hot path analysis

### Slint UI freeze
1. Check event loop não blocked
2. Verify tokio task không deadlock với Slint thread
3. Use `tokio-console` cho async runtime debugging

### Soniox WebSocket
1. Check `WSS://stt-rt.soniox.com/transcribe-websocket` reachable từ network
2. Verify API key valid (test via `SonioxProvider::test_connect`)
3. Inspect tracing logs cho reconnect attempts

## Performance Targets (per plan v2 Section 13.1.5)

| Metric | Target |
|---|---|
| Idle RAM | <30MB |
| Recording RAM | <500MB (with Whisper Base, future Phase) |
| First-token latency Cloud | <500ms |
| First-token latency Local | <2.5s |
| Binary size | <15MB |
| Audio callback deadline | <5ms |
| VAD frame compute | <100µs |
| FTS5 query 1000 sessions | <100ms |

## Validation Gates Before Code

Per plan.md status — these MUST be done before continuing past Phase 0:
1. ✋ Slint license decision (email Slint sales)
2. ✋ 5-10 user interviews completed
3. ✋ 6 spike tests passed (clock drift, Soniox VN WER, FTS5 search, etc.)
4. ✋ Privacy Policy lawyer reviewed

Current status: **SKIPPED per Path C YOLO user choice**. Risk accepted.

## Resources

- Plan v2: `../plans/t-p-trung-v-nghi-n-greedy-pixel.md`
- MVP 0.1 plan: `../plans/260517-1500-vong-stt-mvp01/plan.md`
- Phase files: `../plans/260517-1500-vong-stt-mvp01/phase-*.md`
- Privacy Policy: `../plans/vong-privacy-policy.md`
- Slint docs: https://docs.slint.dev/
- cpal docs: https://docs.rs/cpal
- Tokio docs: https://docs.rs/tokio
