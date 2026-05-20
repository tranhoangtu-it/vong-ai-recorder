# QA Checklist — Vọng AI Recorder v0.1.0-beta.1

Manual test matrix for a clean Windows 11 VM. Execute in order. Do not skip
cases — later cases depend on state established by earlier ones.

**Target**: zero failures across all 30 cases before publishing the beta.
**Tester**: solo dev (you). No assistants needed.
**Estimated time**: 3-5 hours including VM setup and one round of fixes.

---

## Pre-Test Setup

### VM Specification

| Property | Value |
|---|---|
| Hypervisor | Hyper-V (Win 11 Pro for Workstations has it free) or VirtualBox 7.x |
| VM name | `vong-beta-qa` |
| OS | Windows 11 Pro 24H2 (fresh install — no dev tools, no Rust, no audio drivers tuned) |
| RAM | 8 GB (dynamic) |
| vCPU | 4 |
| Disk | 60 GB VHDX (dynamic) |
| Audio | Hyper-V Synthetic Audio Device (for mic test) |
| Network | NAT via Default Switch (Internet access required for model download) |
| Snapshot | Take `clean-baseline` snapshot after OS setup, before any Vọng install |

### VM OS Setup (one-time)

1. Boot from Windows 11 Pro ISO (download from Microsoft's official media creation tool).
2. At "Sign in with Microsoft": press `Shift+F10` → type `OOBE\BYPASSNRO` → Enter → reboot into offline account creation.
3. Create local account: username `testuser`, no password.
4. Skip all optional telemetry/privacy toggles (set to minimum).
5. Install Windows Updates (Start → Settings → Windows Update → Check for updates) — complete all pending.
6. Snapshot VM as `clean-baseline`.

### Network Pre-check (inside VM)

Open Edge → navigate to https://vong.app/ → confirm page loads.
Navigate to https://huggingface.co/ → confirm accessible (model download source).

**Expected setup time**: ~45 minutes (mostly Windows Update).

---

## Test Cases

Each row format: **Case ID · Description · Pre-condition · Steps · Expected result · Phase**

---

### Section A — Install

| Case | Description | Pre-condition | Steps | Expected | Phase | Pass/Fail | Notes |
|---|---|---|---|---|---|---|---|
| **QA-01** | Landing page loads on clean browser | Fresh VM, Edge with no cache | 1. Open Edge. 2. Navigate to https://vong.app/ | Page renders in < 5 s. No console errors (F12 → Console). Download button visible. | P4 (landing) | | |
| **QA-02** | MSI download from R2 | QA-01 passed | 1. Click "Tải về miễn phí (CPU)". 2. Wait for download to complete. | MSI file appears in `%USERPROFILE%\Downloads\`. File size ~12 MB. | P1 | | |
| **QA-03** | SHA-256 hash verification | QA-02 passed | 1. Open PowerShell. 2. Run: `Get-FileHash "$env:USERPROFILE\Downloads\Vong-AI-Recorder-0.1.0-beta.1-x86_64.msi" -Algorithm SHA256`. 3. Compare output hash against value shown on https://vong.app/. | Hashes match exactly. | P1 | | |
| **QA-04** | SmartScreen click-through | QA-02 passed, MSI not yet run | 1. Double-click the MSI file. 2. Observe SmartScreen dialog. 3. Click "More info". 4. Click "Run anyway". | SmartScreen shows "Unknown publisher" (expected — unsigned). "More info" reveals "Run anyway". Installer starts after click. | P1 | | |
| **QA-05** | Per-user install — no UAC prompt | QA-04 passed | 1. Observe the installer window during install. 2. Watch for UAC (blue shield / elevation dialog). | No UAC prompt appears at any point during install. | P1 | | |
| **QA-06** | Install path verification | QA-05 passed | 1. Open File Explorer. 2. Navigate to `%LocalAppData%\Programs\Vong AI Recorder\`. | Folder exists. `vong.exe` present. Size ~12 MB (CPU build). | P1 | | |
| **QA-07** | Start Menu entry | QA-06 passed | 1. Press Windows key. 2. Type "Vọng". | "Vọng AI Recorder" appears in search results. Clicking launches the app. | P1 | | |
| **QA-08** | System tray icon | QA-07, app launched | 1. Look at the system tray (bottom-right of taskbar). | Violet "V" tray icon appears. Right-click shows menu: Show, Hide, Quit. | P3 (tray) | | |

---

### Section B — First-Run Wizard

| Case | Description | Pre-condition | Steps | Expected | Phase | Pass/Fail | Notes |
|---|---|---|---|---|---|---|---|
| **QA-09** | Wizard appears on first launch | Fresh install, no `onboarded.txt` | 1. Launch app (QA-07). | Wizard overlay appears. Step 1 "Chào mừng" shown. Step indicator shows 1/7. | P3 (wizard) | | |
| **QA-10** | Wizard Step 1 — Welcome + privacy | QA-09 | 1. Read the Welcome screen. 2. Confirm SmartScreen explainer text present. 3. Click "Tiếp theo". | Screen mentions audio stays on device (privacy promise). SmartScreen workaround explained. Advances to Step 2. | P3 | | |
| **QA-11** | Wizard Step 2 — Audio source picker | QA-10 | 1. Observe audio sources listed. 2. Select "Microphone (default)". 3. Click "Tiếp theo". | At least one microphone entry listed. Loopback (Speakers) entry also listed. Selection persists. Advances to Step 3. | P1 + P3 | | |
| **QA-12** | Wizard Step 3 — Provider selection (Whisper Local) | QA-11 | 1. Confirm "Whisper Local" is pre-selected / recommended. 2. Click "Tiếp theo". | Whisper Local card shows "Miễn phí · Offline". Provider selection persists. Step 4 (Model Download) appears next (not API key step). | P2 + P3 | | |
| **QA-13** | Wizard Step 4 — Model download (base, ~148 MB) | QA-12 | 1. Confirm "base" model is pre-selected. 2. Click "Tải xuống". 3. Watch progress bar. 4. Wait for completion (~2-5 min depending on connection). | Progress bar animates. ETA shown. On completion: "✓ Đã tải xong" (or similar). SHA-256 verified silently. "Tiếp theo" button becomes active. | P2 + P3 | | |
| **QA-14** | Wizard Step 5 — Language selection | QA-13 | 1. Observe source language picker. 2. Leave default (auto-detect). 3. Set target mode to "Dịch sang tiếng Anh". 4. Click "Tiếp theo". | Source language dropdown shows options including "Tự động nhận diện". Target mode dropdown shows "Dịch sang tiếng Anh", "Hint", "Tắt". Advances to Step 6. | P3 | | |
| **QA-15** | Wizard Step 6 — Done | QA-14 | 1. Observe Done screen. 2. Click "Bắt đầu ghi âm". | Done screen shows a recap (provider, model, language). CTA button visible. Clicking closes wizard and shows main view. | P3 | | |
| **QA-16** | `onboarded.txt` persistence check | QA-15, wizard complete | 1. Open PowerShell. 2. Run: `Get-Content "$env:APPDATA\Vong\Vong\config\onboarded.txt"` | File exists. Contains `v=2` header and `step.complete=<timestamp>` line. | P3 | | |

---

### Section C — Recording

| Case | Description | Pre-condition | Steps | Expected | Phase | Pass/Fail | Notes |
|---|---|---|---|---|---|---|---|
| **QA-17** | Mic recording — streaming partials | Wizard complete, app on main view | 1. Click the mic button (center of main view) to start recording. 2. Speak clearly in Vietnamese for ~10 seconds: "Xin chào, tôi đang kiểm tra ứng dụng Vọng". 3. Pause speaking for 2+ seconds to trigger pack. | Within ~1.5 s of speaking: partial text appears in left "Bản gốc" column. After silence: final text settles + English translation appears in right "Bản dịch" column. Peak meter strip responds during speech. | P1 + P3 | | |
| **QA-18** | Utterance count increments | QA-17 | 1. Make 3 separate utterances (speak → pause → speak → pause → speak → pause). | A visible utterance counter or session row count increments after each final pack. DB must have rows — verify via QA-27. | P1 + P3 | | |
| **QA-19** | Loopback recording | App running | 1. Go to Settings → Recording (or audio source picker in main view). 2. Switch source to "Speakers (loopback)". 3. Open Edge → play a Vietnamese YouTube video at medium volume. 4. Watch transcript columns. | After source switch: loopback is active (peak meter responds to YouTube audio). Transcript text from the video appears in left column. No restart required for source switch. | P1 (loopback) + P3 | | |
| **QA-20** | Stop recording | QA-17 or QA-19 active | 1. Click the mic button again to stop recording. | Recording stops. Peak meter goes flat. No further partials emitted. Session row finalized in DB. | P1 + P3 | | |

---

### Section D — History, Search, Export

| Case | Description | Pre-condition | Steps | Expected | Phase | Pass/Fail | Notes |
|---|---|---|---|---|---|---|---|
| **QA-21** | History sidebar shows session | QA-20 (session completed) | 1. Open History sidebar (left panel). | At least one session row visible. Row shows timestamp, duration, audio source. | P3 (history) | | |
| **QA-22** | Full-text search — exact match | QA-17 transcript contains "xin chào" | 1. Click search field in History sidebar. 2. Type "xin chào". | Session row containing the phrase is highlighted / filtered in results. | P3 (FTS5) | | |
| **QA-23** | Tone-insensitive search | QA-17 transcript contains "Vọng" | 1. In search field, type "vong" (no diacritics). | Same session appears in results. "vong" matches "Vọng" via FTS5 `remove_diacritics 2`. | P3 (FTS5) | | |
| **QA-24** | Known FTS5 `đ` limitation | QA-17 transcript contains "đang" | 1. In search field, type "dang" (no `đ`). 2. Observe no results. 3. Type "đang". 4. Observe results. | "dang" returns no match (expected — known issue I-12). "đang" returns the session (correct). Document in Notes column. | P3 (FTS5) | | |
| **QA-25** | Markdown export | QA-21, session visible in history | 1. Right-click (or use context menu) on a session row. 2. Select "Xuất Markdown". | File created at `%USERPROFILE%\Documents\Vong\session-N.md`. File contains transcript text in readable format. | P3 (export) | | |
| **QA-26** | Export file content check | QA-25 | 1. Open the exported `.md` file in Notepad. | File contains: session metadata (date, duration, source), transcript lines (original + translation), properly encoded Vietnamese characters (not mojibake). | P3 | | |
| **QA-27** | SQLite DB row verification | QA-18 (3 utterances made) | 1. Open PowerShell. 2. Run: `dir "$env:APPDATA\Vong\Vong\data\db.sqlite3"` to confirm file exists. Note size > 0. | DB file exists and is non-zero. (Optional: use DB Browser for SQLite to inspect `transcript_segments` rows if tool available.) | P3 (storage) | | |

---

### Section E — Persistence and Recovery

| Case | Description | Pre-condition | Steps | Expected | Phase | Pass/Fail | Notes |
|---|---|---|---|---|---|---|---|
| **QA-28** | Session persists across app restart | QA-21 (session in history) | 1. Quit app (tray → Quit or close window). 2. Relaunch via Start Menu. | App opens directly to main view (wizard does NOT re-appear). Previous session(s) visible in History sidebar. | P3 (wizard persistence + storage) | | |
| **QA-29** | Provider swap + restart banner | App running | 1. Open Settings → System. 2. Change Provider to "Soniox" (no key needed to test UI). 3. Observe the app. | A restart banner appears: "Khởi động lại Vọng AI Recorder để áp dụng" (or similar). Provider selection was written to `%APPDATA%\Vong\Vong\config\provider.txt`. | P2/P3 | | |

---

### Section F — Uninstall

| Case | Description | Pre-condition | Steps | Expected | Phase | Pass/Fail | Notes |
|---|---|---|---|---|---|---|---|
| **QA-30** | Uninstall — binary removed, data preserved | QA-28, app closed | 1. Open Settings → Apps → Installed apps. 2. Find "Vọng AI Recorder". 3. Click Uninstall. 4. Confirm. 5. After completion, check: `%LocalAppData%\Programs\Vong AI Recorder\` — should be gone. 6. Check: `%AppData%\Vong\Vong\` — should still exist with DB and config. | Binary folder is removed. `vong.exe` no longer present. Start Menu entry gone. AppData folder with `db.sqlite3`, `config\`, `logs\` still exists (user data preserved). | P1 (WiX uninstall) | | |

---

## Total Case Count: 30

| Section | Cases | Focus area |
|---|---|---|
| A — Install | QA-01 to QA-08 | MSI download, SmartScreen, path, tray |
| B — First-run wizard | QA-09 to QA-16 | All 7 wizard steps, persistence |
| C — Recording | QA-17 to QA-20 | Mic, loopback, partials, translation |
| D — History/Search/Export | QA-21 to QA-27 | FTS5 search, export, DB |
| E — Persistence/Recovery | QA-28 to QA-29 | Restart, provider swap |
| F — Uninstall | QA-30 | Data preservation |

---

## Scoring

| Result | Meaning |
|---|---|
| All 30 Pass | GO — publish beta |
| ≤ 2 Fail (Info severity only, workaround documented) | GO WITH WARNING — document in known-issues |
| Any Fail in QA-04/05/06/09/13/17/28/30 | NO-GO — these are critical path |
| Any crash / data loss / DB corruption | NO-GO — fix before proceeding |

---

## Go/No-Go Decision

Record your decision here after completing the matrix:

```
Date executed:
Tester:
VM snapshot used:
Cases passed: __ / 30
Cases failed: __ / 30
Failed case IDs:
Decision: GO / NO-GO
Rationale:
```

---

## Phase Cross-Reference

| Phase | Cases it covers |
|---|---|
| P1 — Installer (WiX MSI, cpal capture, loopback) | QA-02, QA-03, QA-04, QA-05, QA-06, QA-07, QA-08, QA-11, QA-19 |
| P2 — Model downloader (reqwest, SHA-256, wizard wiring) | QA-12, QA-13, QA-29 |
| P3 — Wizard + Settings + History + Storage + Export | QA-09 to QA-16, QA-21 to QA-30 |
| P4 — Whisper local STT + Vulkan + TargetMode | QA-17, QA-18 (translation output) |
