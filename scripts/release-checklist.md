# Release Checklist — Vọng AI Recorder

Pre-tag QA checklist + tag-cut runbook. Complete every item in order before pushing a real version tag.

---

## Part 1 — Pre-Tag QA (local machine)

### 1.1 Version consistency check

Verify the version in `Cargo.toml` matches the intended release tag. The workspace version is the single source of truth — `cargo wix` reads it to name the MSI.

```powershell
# In app/ directory
Select-String -Path Cargo.toml -Pattern '^version'
# Expected output: version = "0.1.0-beta.1"
```

The git tag you push (`v0.1.0-beta.1`) must match with a leading `v` prefix.

### 1.2 Update CHANGELOG.md

Move the `[Unreleased]` section header to the new version + date, then add a fresh `[Unreleased]` placeholder at the top.

```markdown
<!-- Before -->
## [Unreleased]
### Added
- ...

<!-- After -->
## [Unreleased]

## [v0.1.0-beta.1] - 2026-05-20
### Added
- ...
```

Commit this change **before** pushing the tag so the release notes are accurate.

### 1.3 Run tests (no failures allowed)

```powershell
cd "E:\AgentAI\AI Translate\app"
cargo test --workspace --lib
# Expected: all tests pass (currently 107 tests)
```

If any test fails: fix it, do not push the tag.

### 1.4 Local release build sanity check

```powershell
cargo build --release --bin vong

# Verify binary size is under 15 MB
$size = (Get-Item target\release\vong.exe).Length
Write-Output "$([math]::Round($size/1MB,1)) MB"
# Must be < 15.0 MB
```

### 1.5 Local MSI build (requires WiX Toolset v3 installed)

```powershell
# Requires: choco install wixtoolset (one-time)
# Requires: cargo install cargo-wix --locked --version 0.3.9 (one-time)

cargo wix --package vong-app --no-build --nocapture
# Expected: target/wix/vong-app-0.1.0-beta.1-x86_64.msi created
```

If WiX is not installed locally, skip this step — CI will build it. But you should smoke-test MSI from CI before publishing the release.

### 1.6 Smoke-test the MSI installer

If you built the MSI locally (or downloaded from a CI dry-run):

- [ ] Double-click MSI — Windows SmartScreen warning appears (expected for unsigned binary)
- [ ] Click "More info" → "Run anyway" — installer launches, no UAC prompt
- [ ] App installs to `%LOCALAPPDATA%\Programs\Vong AI Recorder\`
- [ ] Start Menu shortcut appears: "Vọng AI Recorder"
- [ ] App launches, mic/loopback capture works
- [ ] Transcription produces output with default Whisper local provider
- [ ] Settings screen opens, tabs render correctly
- [ ] Uninstall via Programs & Features — app removed, user data in `%APPDATA%\Vong\` preserved

### 1.7 Verify GitHub Secrets are set

Go to: **GitHub → vong-ai-recorder → Settings → Secrets and variables → Actions**

Confirm all 5 secrets exist (values are hidden — just confirm the names):

- [ ] `R2_ACCOUNT_ID`
- [ ] `R2_ACCESS_KEY_ID`
- [ ] `R2_SECRET_ACCESS_KEY`
- [ ] `R2_BUCKET`
- [ ] `R2_PUBLIC_BASE`

If any are missing: see `.github/r2-setup.md`.

---

## Part 2 — Dry Run (first time only)

Run the full pipeline once with a dummy tag to verify R2 upload + GitHub Release creation work end-to-end. Skip this if you've already done a successful dry run.

### 2.1 Push dummy tag

```powershell
cd "E:\AgentAI\AI Translate\app"
git tag v0.0.0-beta.0
git push origin v0.0.0-beta.0
```

### 2.2 Monitor CI

Go to: **GitHub → vong-ai-recorder → Actions → Release**

Wait ~12–15 min. Verify:

- [ ] `Build CPU MSI` job: green
- [ ] `Build Vulkan MSI` job: green (or yellow/skipped — continue-on-error, won't block CPU release)
- [ ] `Publish to R2 + GitHub` job: green
- [ ] Step summary shows R2 download URLs and SHA-256 hashes

### 2.3 Verify R2 objects

```powershell
$env:AWS_ACCESS_KEY_ID     = "<your-key>"
$env:AWS_SECRET_ACCESS_KEY = "<your-secret>"
$env:AWS_DEFAULT_REGION    = "auto"
$env:AWS_ENDPOINT_URL      = "https://<account-id>.r2.cloudflarestorage.com"

aws s3 ls "s3://vong-releases/releases/v0.0.0-beta.0/"
# Should list: 2 MSI files + SHA256SUMS
```

### 2.4 Verify public download URL

```powershell
# Download and verify hash
$url = "https://dl.vong.app/releases/v0.0.0-beta.0/Vong-AI-Recorder-0.0.0-beta.0-x86_64.msi"
Invoke-WebRequest $url -OutFile test-download.msi
Get-FileHash test-download.msi -Algorithm SHA256
# Compare with SHA256SUMS content from R2
```

### 2.5 Cleanup dry run artifacts

```powershell
# Run the cleanup script (see scripts/release-r2-cleanup.ps1)
.\scripts\release-r2-cleanup.ps1 -Tag "v0.0.0-beta.0"

# Also delete the draft GitHub Release manually:
# GitHub → Releases → v0.0.0-beta.0 → Edit → Delete this release
```

---

## Part 3 — Real Release Tag

Complete Part 1 and Part 2 before this section.

### 3.1 Commit CHANGELOG update

```powershell
cd "E:\AgentAI\AI Translate\app"
git add CHANGELOG.md
git commit -m "chore(release): update changelog for v0.1.0-beta.1"
git push origin master
```

### 3.2 Push the release tag

```powershell
git tag v0.1.0-beta.1
git push origin v0.1.0-beta.1
```

This triggers the pipeline. Monitor at: **GitHub → Actions → Release**

### 3.3 Wait for pipeline completion (~12–15 min)

- [ ] `Build CPU MSI`: green
- [ ] `Build Vulkan MSI`: green (or yellow — non-blocking)
- [ ] `Publish to R2 + GitHub`: green
- [ ] Step summary shows correct URLs and hashes

### 3.4 Smoke-test the real MSI from R2

Download the CPU MSI from the URL in the step summary and verify:

```powershell
# URL from step summary, e.g.:
$url = "https://dl.vong.app/releases/v0.1.0-beta.1/Vong-AI-Recorder-0.1.0-beta.1-x86_64.msi"
Invoke-WebRequest $url -OutFile "Vong-AI-Recorder-0.1.0-beta.1-x86_64.msi"

# Verify hash matches SHA256SUMS
$hash = (Get-FileHash "Vong-AI-Recorder-0.1.0-beta.1-x86_64.msi" -Algorithm SHA256).Hash.ToLower()
Write-Output $hash
# Compare to: https://dl.vong.app/releases/v0.1.0-beta.1/SHA256SUMS
```

Run the MSI on a clean Windows VM if possible (at minimum on your dev machine).

### 3.5 Publish the GitHub Release draft

1. Go to: **GitHub → vong-ai-recorder → Releases**
2. Find the `v0.1.0-beta.1` draft release
3. Review the auto-generated release notes — edit if needed
4. Click **Publish release**

The release is now visible to all repo collaborators (repo is private — not public-facing).

### 3.6 Update landing page

Copy the R2 URLs and SHA-256 hashes from the workflow step summary into the landing page Download section. Replace the Phase 4 placeholder values with real values.

Fields to update:
- CPU MSI download URL
- Vulkan MSI download URL
- CPU SHA-256 hash
- Vulkan SHA-256 hash
- Release date

### 3.7 Announce to beta testers

Share the landing page download link or the direct R2 URL with beta testers. The GitHub Release is for developer/collaborator reference only (private repo).

---

## Quick Reference — Tag Commands

```powershell
# Check existing tags
git tag --list "v*" | Sort-Object

# Create and push tag
git tag v0.1.0-beta.1
git push origin v0.1.0-beta.1

# Delete a tag (local + remote) — for cleanup only
git tag -d v0.1.0-beta.1
git push --delete origin v0.1.0-beta.1

# Trigger pipeline manually (without a tag, dry_run=false)
gh workflow run release.yml
# Or with dry run:
gh workflow run release.yml -f dry_run=true
```

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `AccessDenied` on R2 upload | Wrong `R2_ACCESS_KEY_ID` / `R2_SECRET_ACCESS_KEY` secrets | Re-check secrets in GitHub Settings |
| `NoSuchBucket` on R2 upload | `R2_BUCKET` secret has wrong value | Verify bucket name is exactly `vong-releases` |
| `Endpoint URL does not exist` | `R2_ACCOUNT_ID` wrong | Copy 32-char hex from Cloudflare dashboard URL |
| Vulkan build job fails | Vulkan SDK version mismatch or Ninja not in PATH | Non-blocking — CPU release still ships. Investigate separately. |
| Binary size >15 MB | New large dependency added | Profile with `cargo bloat --release`, remove or feature-gate |
| MSI not found after `cargo wix` | `main.wxs` template issue | Check `vong-app/wix/main.wxs` exists and is valid |
| GitHub Release not created | `contents: write` permission missing | Verify `permissions: contents: write` in workflow |
| Download URL 404 | DNS not propagated or R2 public access not enabled | Use `pub-<id>.r2.dev` fallback URL temporarily |
