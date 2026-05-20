# Beta Tester Recruitment — Vọng AI Recorder v0.1.0-beta.1

Target: **30 sign-ups, first-come-first-served**. Expect ~20-30% activation
(6-9 people actively filing feedback). That is enough signal for Sprint 2.

---

## Ideal Beta Tester Profile

A "good" tester for Vọng beta is someone who:

| Criterion | Why it matters |
|---|---|
| Uses a Vietnamese keyboard (Unikey, EVKey, or built-in IME) | Exercises the FTS5 tone-insensitive search and diacritic edge cases |
| Attends multilingual meetings or calls at least weekly | Represents the core use case — mix of Vietnamese + English speech |
| Is comfortable installing unsigned software (will click "More info → Run anyway" in SmartScreen) | Eliminates drop-off at the first friction point |
| Runs Windows 10 or 11 on their primary machine | Target OS — no macOS testers needed in Sprint 1 |
| Willing to file structured feedback (not just "it doesn't work") | Actionable signal requires reproduction steps |
| Has a stable Internet connection (≥ 10 Mbps) for the ~148 MB model download | Completes the wizard successfully |

**Nice to have** (bonus signal):
- Has a discrete GPU (NVIDIA or AMD) — can test Vulkan build performance.
- Works at a remote-first Vietnamese startup or international company.
- Has used Whisper, Otter.ai, Fireflies, or similar tools — can compare.
- Creates content (YouTube, podcast, blog) in Vietnamese or bilingual.

**Avoid**:
- IT administrators who will block unsigned installers at the domain level.
- Users on Windows 7/8 (not supported, wastes both parties' time).
- Non-Windows users (redirect them to the waitlist for a future macOS build).

---

## Recruitment Channels

### Tier 1 — Vietnamese Developer Communities (highest signal density)

These communities have members who are both technically capable and likely to
use a transcription tool. Post here first.

**Facebook Groups**

Note: Facebook group membership fluctuates. Verify the group is still active
(last post within 7 days) before posting. Do not post the same message to
more than 2-3 groups simultaneously to avoid appearing spammy.

| Category | What to look for | Notes |
|---|---|---|
| Large general VN developer community | Groups with "Lập trình", "Developer", "Coder" in the name, 100k+ members | Highest reach; mixed technical level |
| VN remote work / freelancer community | Groups focused on remote work, "làm việc từ xa", digital nomad Vietnam | Direct use-case match for multilingual calls |
| VN startup / product builder community | Groups for founders, indie makers, product people | Early-adopter mindset; likely to give quality feedback |
| J2TEAM Community | Specific group — large VN tech community run by J2TeaM | Known for engaged technical members |
| Cộng đồng Lập trình Viên Việt Nam | Specific group — one of the largest VN dev communities on Facebook | Good for reaching working developers |

**Posting strategy for Facebook**:
1. Post from your personal profile (not a page) — personal posts get more organic reach.
2. Tag 2-3 friends who you know are in the target audience and ask them to comment.
3. Use the Vietnamese announcement post from `beta-announcement-vi.md`.
4. Pin the post to your profile for 7 days.
5. Reply to every comment within 24 hours — engagement boosts algorithmic reach.

---

**Voz Forum (Vozforums.com)**

- Subforum: **Hardware & Software** (Phần cứng & Phần mềm) — large audience for desktop app reviews.
- Post format: thread title should be descriptive, not clickbait: "Vọng AI Recorder — ứng dụng ghi âm + dịch nói offline Windows, tìm beta tester".
- Include screenshots (use the placeholder screenshots from the landing page until real ones are ready).
- Voz users are technically savvy and will test edge cases — good for quality signal.

---

### Tier 2 — Reddit (English-friendly, VN diaspora reach)

| Community | Audience | Post approach |
|---|---|---|
| r/vietnam | Vietnamese people globally, mix of expats + locals | Use the English announcement (`beta-announcement-en.md`); mention Vietnamese-first focus |
| r/learnvietnamese | People studying Vietnamese — multilingual, will appreciate the translation feature | Frame around "tool that helped me follow Vietnamese meetings" |
| r/selfhosted | Privacy-conscious users who prefer offline/local AI tools | Lead with "100% offline Whisper, no cloud, AGPL" angle |
| r/rust (if posting about tech stack) | Rust developers globally | Frame as a Rust + Slint desktop app showcase, mention beta testers needed |

**Reddit rules**: Read community rules before posting. r/vietnam allows personal
projects if framed helpfully. Avoid pure self-promotion framing — lead with
the problem the app solves.

---

### Tier 3 — Personal Network (highest conversion rate, lowest volume)

Your personal network will give the most honest and detailed feedback even
if the numbers are smaller.

**Plays that work for a solo dev with a modest network**:

1. **Direct message 5-10 friends/colleagues** who work at remote-first
   companies or attend multilingual calls. A personal DM converts at ~60-80%
   vs ~5% for a public post. Message template:

   > "Mình vừa build xong ứng dụng ghi âm + dịch nói cho Windows, chạy
   > offline với Whisper. Đang tìm người test thử — bạn có hay họp online
   > với người nước ngoài không? 15 phút cài thử và cho mình biết cảm nhận
   > là mình cảm ơn nhiều lắm. Link: https://vong.app/"

2. **Ask 3-5 people to reshare** your Facebook / LinkedIn post. One reshare
   from a person with 500+ followers can double your reach. Be specific in
   the ask: "Bạn có thể share post này không, mình đang tìm người test thử?"

3. **Multilingual content creators**: Vietnamese YouTubers, podcasters, or
   TikTokers who produce bilingual content are perfect testers. Search
   YouTube/TikTok for Vietnamese channels that mix Vietnamese + English. DM
   them with a genuine pitch (not a template).

4. **Vietnamese remote workers on LinkedIn**: Search for "Vietnam" + "remote"
   + job titles like "Software Engineer", "Product Manager", "Content Creator".
   Connect and send a short personalised note.

5. **University alumni groups**: If you have a university alumni WeChat/Zalo/
   Facebook group, post there. Graduates working in tech or international
   companies are ideal.

---

### Tier 4 — Tech Blogs / Newsletters (slower burn, long tail)

- **Kipalog** (kipalog.com) — Vietnamese developer blog platform. Write a
  technical post about building Vọng with Rust + Slint + Whisper. Include
  beta sign-up link at the end. This drives high-quality tester sign-ups.
- **dev.to** — Use `beta-announcement-en.md` as a starting point. Add tags:
  `#rust`, `#windows`, `#opensource`, `#ai`.
- **Substack / personal blog**: If you have a newsletter, announce there first.
  Existing subscribers are the most likely to activate.

---

## Sign-Up Process

### Recommended: Google Form

Set up the Google Form per `feedback-channels.md`. Place the link:
- In the beta announcement posts (primary CTA).
- On https://vong.app/ footer ("Tham gia beta").
- In your Telegram / Zalo bio for the duration of beta.

### Form Fields for Screening

The following fields in the Google Form double as screening questions:

| Field | What you learn from the answer |
|---|---|
| Tên + Telegram/Email | Contact for follow-up; Telegram = higher engagement likelihood |
| Phiên bản Windows | Filter out Windows 7/8/macOS non-starters |
| Bạn có GPU rời không? (NVIDIA/AMD) | Route GPU-capable testers to Vulkan build instructions |
| Bạn thường họp online bằng ngôn ngữ nào? | Confirms multilingual use case; single-language = lower-value tester |
| Mô tả một tình huống bạn muốn dùng Vọng | Reveals genuine use case vs curiosity; 1-sentence answer is sufficient |
| Bạn có sẵn sàng cài phần mềm chưa được ký số (unsigned)? | Hard filter — "Không" = politely decline or redirect to waitlist |

### After Sign-Up

1. Within 24 hours: send a personalised welcome email/Telegram message with:
   - Download link (https://vong.app/).
   - Known issues list link (`docs/known-issues-beta.md` or the landing FAQ).
   - Feedback form link (the same Google Form, or a separate per-session form).
   - Telegram group invite link (if they opted in).

2. At Day 3: send a short check-in: "Bạn đã cài được chưa? Có vấn đề gì không?"
   Many installs fail silently — a check-in recovers testers who got stuck.

3. At Day 7: send the weekly digest with what was fixed based on their feedback.

---

## Recruitment Cap and Pacing

- **Hard cap**: 30 sign-ups. After 30, thank people and add to a waitlist
  for Sprint 2 beta.
- **Pacing**: Post to Tier 1 (Facebook + Voz) on Day 0. Wait 3 days for
  sign-ups. If < 10 sign-ups: also post to Tier 2 (Reddit). If still < 10
  after Day 7: activate Tier 3 (personal DMs).
- **Do not flood all channels simultaneously** on Day 0 — you want to be
  available to respond to questions and triage early feedback before the
  next wave arrives.

---

## Expected Conversion Funnel

| Stage | Estimate |
|---|---|
| Post impressions (Facebook + Voz combined) | 500-2,000 |
| Click-through to https://vong.app/ | 50-200 (10% CTR) |
| Start form sign-up | 30-60 (60% of clicks) |
| Complete form | 20-40 (70% completion) |
| Actually install the app | 10-20 (50% activation) |
| File at least one piece of feedback | 6-12 (60% of installers) |

Target: **6-12 active testers filing feedback** in the first 14 days.
This is enough signal to validate the core flow and identify top-3 issues
for Sprint 2 planning.
