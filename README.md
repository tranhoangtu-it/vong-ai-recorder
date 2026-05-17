# Vọng STT

> Cuộc họp đa ngôn ngữ. Không một câu bị bỏ sót.

Desktop Speech-to-Text app cho macOS với real-time multilingual translation, mic + system audio mix, privacy-first BYOK architecture.

**Status**: 🚧 Phase 0 — Scaffolding (Alpha). MVP 0.1 beta tháng 8/2026.

## Quick Start (Dev)

### Prerequisites
- Rust 1.78+ (`rustup install stable`)
- macOS 13.0+ (Apple Silicon hoặc Intel) — primary dev target
- Windows: cross-compile only (cannot run macOS features)

### Build

```bash
cd app
cargo build --release
./target/release/vong  # macOS only — opens Phase 0 Slint window
```

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
vong-app/      Main binary — Slint UI entry point
vong-audio/    Audio capture + mix + VAD (Phase 1-3)
vong-stt/      Speech-to-text providers (Phase 4: Soniox WS)
vong-storage/  SQLite + FTS5 Vietnamese search (Phase 6)
vong-ui/       Tray + Floating Pill + Dashboard (Phase 5+7)
```

## Roadmap

| Phase | Status | Effort | Description |
|---|---|---|---|
| 0 | 🚧 In Progress | 4-6h | Scaffolding + tooling |
| 1 | Pending | 12-20h | Mic capture (cpal + RT) |
| 2 | Pending | 40-60h | Loopback + clock drift |
| 3 | Pending | 4-8h | VAD + state machine |
| 4 | Pending | 12-24h | Soniox WebSocket |
| 5 | Pending | 24-32h | Tray + Pill + Hotkey |
| 6 | Pending | 10-15h | SQLite FTS5 VN search |
| 7 | Pending | 8-12h | Onboarding + permissions |
| 8 | Pending | 16-24h | DMG + signing + beta |

Total: ~12 weeks side-project. Detail trong `plans/` directory.

## Privacy & Security

- Audio NEVER leaves your machine to Vọng servers (BYOK direct to provider)
- API keys stored in macOS Keychain via `keyring` crate
- Logs sanitized (no audio content, no API keys, no user data)
- Tuân thủ Luật Bảo vệ Dữ liệu Cá nhân Việt Nam 91/2025/QH15
- See [SECURITY.md](./SECURITY.md) for vulnerability disclosure

## License

AGPL-3.0-only cho OSS core. Pro tier (future) sẽ proprietary.

⚠️ Slint dual-licensed (GPL-3.0 / Commercial). Sử dụng Slint GPL-3.0 cho free tier. Pro tier require Slint Royalty-free Commercial license — pending decision (plan v2 blocker #1).

## Documentation

- [Development guide](./docs/development.md)
- [Implementation plan](../plans/260517-1500-vong-stt-mvp01/plan.md) (10-12 weeks)
- [Design doc v2](../plans/t-p-trung-v-nghi-n-greedy-pixel.md) (2046 lines, 7 review passes)
- [Privacy Policy](../plans/vong-privacy-policy.md) (bilingual VN/EN)

## Contact

- Email: `dev@vong.app`
- Security: `security@vong.app`
- Privacy: `privacy@vong.app`
