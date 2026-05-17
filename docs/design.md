# Vọng — Brand & UI Design v0

> Phase 0 retroactive design doc. Locks brand name, color/typography system, and
> wireframes the 5 MVP 0.1 screens. Implementation lives in Phase 5 (tray + pill +
> dashboard) and Phase 7 (onboarding) — this doc is the visual contract those phases
> must honor.

## 1. Brand

### Name: Vọng

| Lens | Reasoning |
|---|---|
| Etymology (vi) | "Vọng" = vang vọng, hồi âm, mong đợi. Audio app → resonance fit. |
| Phonetic | Single syllable, low tone (`̣`), 4 chars including diacritic. Memorable. |
| Tagline (vi) | "Cuộc họp đa ngôn ngữ. Không một câu bị bỏ sót." |
| Tagline (en, draft) | "Every word, every language, every meeting." |
| Domain | `vong.app` (claimed). Email handles `dev@`, `security@`, `privacy@`. |
| Touchpoints already locked | Cargo authors `Vọng <dev@vong.app>`, binary `vong`, log namespace `vong`, Keychain service `com.vong.stt`, repo slug `vong-stt-mvp01`. |
| Alternatives considered (and rejected) | `Echo` — trademark conflicts (Amazon); `Loa` — too literal "speaker"; `Tiếng` — too generic "voice"; `Phiên` — overloaded "session/translate"; `Ngân` — risks confusion with "bank"; `Vng` (no diacritic) — kills the Vietnamese identity. |
| Risk | Diacritic in name → some build chains mangle it (CI grep, file naming). Mitigated: package name `vong-stt`, binary `vong` use ASCII; display name `Vọng` only in UI strings + Cargo `description`. |

**Decision: LOCKED.** Do not re-open without explicit user trigger.

### Visual identity

- **Primary accent**: `#8B5CF6` (violet 500) — already in Phase 0 Slint hello.
  Rationale: distinct from blue-saturated incumbents (Zoom blue, Otter teal),
  feminine + tech, prints well on both dark and light surfaces.
- **Secondary accent**: `#4ADE80` (green 400) — recording-active / success state.
- **Danger**: `#F87171` (red 400) — drop / mic-error / overflow.
- **Surfaces (dark theme)**:
  - `#0A0A0A` — app background (true near-black, OLED-friendly)
  - `#171717` — card / panel surface
  - `#262626` — hover / pressed surface
  - `#404040` — border / separator
- **Text**:
  - `#FAFAFA` — primary
  - `#A3A3A3` — secondary
  - `#737373` — disabled / hint
- **Light theme**: deferred to MVP 0.2. Dark only for MVP 0.1.

### Typography

- **Family**: System stack — SF Pro on macOS, Segoe UI Variable on Windows,
  Inter on Linux. Slint `font-family` left empty → uses platform default.
- **Scale** (rem-equivalent, Slint uses px):
  - `48px` — display (logo wordmark only)
  - `28px` — H1 (dashboard headline)
  - `20px` — H2 (section)
  - `16px` — body / live transcript
  - `14px` — secondary body / metadata
  - `12px` — caption / timestamp / status
- **Weights**: 400 regular, 600 semibold, 800 black (logo only).

### Logo / icon

ASCII placeholder, real asset arrives Phase 8:

```
 ┌────────────┐
 │   🔊       │  Tray icon size: 22×22 macOS, 16×16 Windows
 │  V ọ n g   │  Wordmark: 48px @ 800 weight, violet
 │            │  Icon-only: speaker glyph + 3 sound waves
 └────────────┘
```

Tray states (Phase 5):
- **Idle** — outlined speaker, neutral grey
- **Recording** — filled speaker + violet pulse animation @ 1Hz
- **Paused** — filled speaker, no pulse
- **Error** — speaker with red overlay dot

## 2. Screen inventory

5 screens for MVP 0.1. Sized for 1440×900 reference (most common dev display).

| ID | Screen | Phase | Window kind | Size |
|---|---|---|---|---|
| S1 | Tray icon + menu | 5 | OS menu bar item + dropdown | 22×22 + 280×auto |
| S2 | Floating Pill | 5 | Always-on-top borderless | 360×120 |
| S3 | Dashboard | 5/6 | Standard window | 960×640 default |
| S4 | History view (Dashboard tab) | 6 | inside S3 | — |
| S5 | Onboarding wizard | 7 | Modal-style window | 720×520 |

## 3. Wireframes

### S1 — Tray menu (Phase 5)

```
                                    ┌─────────────────────────────────┐
                                    │  ● Recording — 02:14            │
                                    │  Cuộc họp với Anh A             │  (current session, if any)
                                    │  ──────────────────             │
                                    │  ⏸  Pause           ⌘⇧P         │
                                    │  ⏹  Stop & Save     ⌘⇧R         │
                                    │  ──────────────────             │
                                    │  ⤴  Open Dashboard              │
                                    │  ⚙  Preferences…                │
                                    │  ──────────────────             │
                                    │  Quit Vọng                      │
                                    └─────────────────────────────────┘
                                                                        ▲ tray icon
                                                                          (idle → menu different)
```

Idle-state menu has only: `▶ Start Recording  ⌘⇧R`, `Open Dashboard`, `Preferences…`, `Quit`.

### S2 — Floating Pill (Phase 5)

```
┌────────────────────────────────────────────────────────┐
│ ● 02:14  🔴 vi → en          ⏸    ⏹    ⤡ Dashboard    │
│                                                        │
│  "...và như anh nói, deadline là cuối tháng này..."    │  ← live transcript
│  "...so as you said, the deadline is end of month..."  │  ← translation if enabled
└────────────────────────────────────────────────────────┘
   ▲                ▲              ▲     ▲     ▲
   recording dot   detected lang   pause stop  expand to dashboard
   timer in mono   + target lang   hotkey hints on hover
```

Acrylic / vibrancy background on macOS (window-vibrancy crate). Position: bottom-center
by default, draggable, sticky to screen edges. Persists position per-user in config.

### S3 — Dashboard main (Phase 5/6)

```
┌──────────────────────────────────────────────────────────────────────────┐
│ 🔊 Vọng                                          [⚙]  [—]  [□]  [✕]      │
├──────┬───────────────────────────────────────────────────────────────────┤
│      │                                                                   │
│ 🏠   │  Today                                                            │
│ Home │                                                                   │
│      │  ┌────────────────────────────────────────────────────────────┐  │
│ 📜   │  │ ● Live — 02:14    vi → en                                  │  │
│ Hist │  │   "...và như anh nói, deadline..."                         │  │
│      │  │   [⏸ Pause] [⏹ Stop & Save]                                │  │
│ 🔎   │  └────────────────────────────────────────────────────────────┘  │
│ Srch │                                                                   │
│      │  Recent                                                           │
│ ⚙   │  ┌────────────────────────────────────────────────────────────┐  │
│ Sets │  │ Cuộc họp với Anh A     14:30  ·  42 min  ·  vi+en  →      │  │
│      │  │ Standup team Backend   09:15  ·  18 min  ·  vi     →      │  │
│      │  │ Interview Product Mgr  Yesterday 16:00 · 1h 02m · vi+en → │  │
│      │  └────────────────────────────────────────────────────────────┘  │
│      │                                                                   │
│      │  [+ Start Recording]                                              │
│      │                                                                   │
└──────┴───────────────────────────────────────────────────────────────────┘
```

Left sidebar 72px wide, icon + 4-char label. Hidden on <840px window width.

### S4 — History view (Phase 6)

```
┌──────────────────────────────────────────────────────────────────────────┐
│ 🔊 Vọng > History                                [⚙]  [—]  [□]  [✕]      │
├──────┬───────────────────────────────────────────────────────────────────┤
│      │  ┌────────────────────────────────────────────────────────────┐  │
│ 🏠   │  │ 🔎  Tìm trong toàn bộ phiên ghi…                            │  │
│      │  └────────────────────────────────────────────────────────────┘  │
│ 📜   │  ─────────────────────────────────────────────────────────────   │
│ ●    │   Today           (3)                                             │
│      │     ▸ Cuộc họp với Anh A      14:30 · 42m · vi+en   42 segments   │
│ 🔎   │     ▸ Standup team Backend    09:15 · 18m · vi      31 segments   │
│      │     ▸ Untitled session        08:02 · 4m  · vi       7 segments   │
│ ⚙   │   Yesterday       (1)                                             │
│      │     ▸ Interview Product Mgr   16:00 · 1h 02m · vi+en  98 segments │
│      │   This week       (5)                                             │
│      │     ▸ …                                                           │
│      │                                                                   │
│      │   Right click row → Export TXT / Markdown / SRT, Delete           │
└──────┴───────────────────────────────────────────────────────────────────┘
```

Search uses FTS5 `unicode61 remove_diacritics 2` — query "viet" matches "Việt", "viết",
"việc". Result rows show matched snippet with `<mark>` highlight (Slint rich text).

### S5 — Onboarding wizard (Phase 7)

5-step flow, 60 seconds target completion:

```
Step 1 / 5   ●○○○○                                                Skip
┌──────────────────────────────────────────────────────────────────┐
│                                                                  │
│                       🔊                                         │
│                                                                  │
│                    Vọng                                          │
│                                                                  │
│       Cuộc họp đa ngôn ngữ.                                      │
│       Không một câu bị bỏ sót.                                   │
│                                                                  │
│       Cài đặt mất khoảng 1 phút.                                 │
│                                                                  │
│                                          [ Bắt đầu → ]           │
└──────────────────────────────────────────────────────────────────┘

Step 2 — Microphone permission     (probe + macOS system prompt trigger)
Step 3 — Soniox API key            (paste field + test connection button)
Step 4 — Hotkey                    (capture chord, default ⌘⇧R)
Step 5 — Done                      ("Press ⌘⇧R to start your first recording")
```

Each step: header (16px), illustration (placeholder rect 240×160), body (14px),
primary CTA (violet button right-aligned), secondary "Skip" link top-right.

## 4. Slint implementation map

| Wireframe | Slint file | Status |
|---|---|---|
| S1 | `vong-ui/ui/tray-menu.slint` (Phase 5) | not created |
| S2 | `vong-ui/ui/floating-pill.slint` (Phase 5) | not created |
| S3 / S4 | `vong-ui/ui/dashboard.slint` (Phase 5/6) | not created |
| S5 | `vong-ui/ui/onboarding.slint` (Phase 7) | not created |
| Phase 0 hello | `vong-app/ui/app-window.slint` | refreshed in this iteration to preview the design tokens |

Color tokens + typography belong in `vong-ui/ui/tokens.slint` (created Phase 5) and
`@import`-ed by all screens. Until Phase 5, the Phase 0 hello duplicates them inline
as the visual reference.

## 5. Accessibility & i18n

- All UI strings in Slint `@tr()` blocks (Slint built-in i18n). Defaults vi, en
  translation files added Phase 7.
- Min contrast ratio 4.5:1 (WCAG AA) for body text on surfaces. Violet `#8B5CF6` on
  `#0A0A0A` = 7.2:1 ✓.
- Floating Pill: keyboard-only operation possible (Tab cycles pause/stop/expand).
- Tray menu items have `accesskey` hints (`⌘⇧R`, etc.).
- Screen reader: Slint exposes accessibility tree via OS-native APIs (NSAccessibility
  on macOS, UI Automation on Windows).

## 6. Open design questions

- **Pill width responsiveness**: live transcript wraps or truncates? → wrap with
  max-height 120px, scroll lock-to-bottom. Decided.
- **Dashboard sidebar collapse**: at what window width? → 840px below = icons only,
  640px below = sidebar hidden (hamburger in header). Decided.
- **Light theme**: deferred MVP 0.2. Don't design tokens yet.
- **Logo SVG**: blocked on illustrator. Use 🔊 emoji as stand-in MVP 0.1.
