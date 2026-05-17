# Security Policy

## Supported Versions

| Version | Supported |
|---|---|
| 0.1.x-alpha | ✅ Active development |
| < 0.1.0 | ❌ Pre-release, not for production |

## Reporting a Vulnerability

⚠️ **Do NOT open public GitHub Issues for security vulnerabilities.**

Email: **security@vong.app**

Optional GPG encryption (key TBD post v0.1 launch):

```
[GPG fingerprint will be published here when generated]
```

### What to include
- Affected component (audio pipeline, STT provider, storage, UI, build)
- Reproduction steps (minimal example preferred)
- Impact assessment (your view of severity)
- Proposed fix (if you have one)

### Response SLA

| Phase | Time |
|---|---|
| Initial triage | 48 hours |
| Acknowledgment | 7 days |
| Plan + fix ETA | 14 days |
| Critical fix | <30 days (where feasible for indie project) |

## Scope

### In scope ✅
- Vọng app binary + dependencies
- Update server (`updates.vong.app`)
- Landing page (`vong.app`)
- API key handling + Keychain storage
- Audio buffer / transcript leak
- Sanitization gaps trong logs / crash reports
- WebSocket TLS configuration

### Out of scope ❌
- 3rd party services (Soniox, OpenAI, Anthropic, Google) — report to them directly
- Social engineering attacks on users
- Physical attacks (device theft)
- Denial-of-service on your own device (e.g., disk full)
- Bugs in unreleased / experimental branches

## Vulnerability Disclosure Philosophy

Vọng is an **indie project** with limited resources. Best-effort response, no
formal bug bounty (yet). We commit to:

- 🤝 Treat reporters respectfully
- 📢 Credit reporters publicly (with permission) trong release notes
- 🔓 Public disclosure 90 days after fix (or coordinated)
- 📋 Track findings in GitHub Security Advisories

## Known Threats (per plan v2 Section 15.1 STRIDE)

| Threat | Status |
|---|---|
| API key exfiltration | Mitigated via `keyring` + `secrecy::Secret<String>` + zeroize-on-drop |
| Update server tampering | Mitigated via Ed25519 signature verify (Phase 8) |
| Crash report leak (audio/transcript) | Mitigated via Sentry `before_send` sanitization (Phase 8) |
| Local DB unauthorized read | Relies on OS-level disk encryption (FileVault) — SQLCipher option Pro tier |

## Privacy Concerns

Privacy concerns (separate from security vulnerabilities):

Email: **privacy@vong.app**

See [Privacy Policy](../plans/vong-privacy-policy.md) for full data handling
disclosure (PDPL 2026 compliant).
