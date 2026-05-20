# Beta Feedback Channels — Vọng AI Recorder v0.1.0-beta.1

Decision document for how to collect and triage feedback from 20-30 beta
testers during Sprint 1 beta. Updated as channels are set up.

---

## Options Evaluated

### Option A — GitHub Issues (public repo)

**How it works**: Make the repo public, direct testers to file issues via
GitHub's standard issue tracker.

| Pros | Cons |
|---|---|
| Testers can search existing issues before filing duplicates | Repo is currently **private** — making it public exposes full commit history |
| Built-in labels, milestones, assignees | AGPL codebase public = anyone can fork under the same name immediately |
| Developers are familiar with the format | Requires GitHub account — friction for non-developer Vietnamese power users |
| Free for public repos | Brand protection: no control over forks using "Vong AI Recorder" trademark |
| Inline code references possible | Legal review recommended before exposing AGPL + commit history publicly |

**Verdict**: Defer to Sprint 2. Going public is a one-way door requiring
deliberate brand and legal readiness. The beta window (2-4 weeks) is too
short to complete that review.

---

### Option B — Google Form (structured intake)

**How it works**: A Google Form at forms.google.com collects structured
reports from any tester. Link placed in the app's About section and on
the landing page footer.

| Pros | Cons |
|---|---|
| Zero infrastructure — set up in 10 minutes | No public discussion — testers can't see each other's reports |
| No account required (just a browser) | No duplicate detection for testers |
| Structured fields enforce useful metadata | Requires manual triage — responses arrive in Google Sheets |
| Screenshot upload built in | No threading or conversation |
| Works for non-technical Vietnamese users | Form URL can be shared beyond intended testers |
| Free, no server costs | Google account required to manage form |
| Vietnamese language support in form UI | |

**Verdict**: Best fit for structured bug intake from the full 20-30 tester
pool. Use as the primary inbound channel.

---

### Option C — Telegram Group (real-time chat)

**How it works**: A private Telegram group, invite-only. Testers join via
link from the Google Form opt-in checkbox.

| Pros | Cons |
|---|---|
| Real-time debugging loop — fastest signal | Unstructured — bugs get buried in conversation |
| Group dynamic motivates testers to stay engaged | Requires Telegram account (most Vietnamese users have it) |
| Voice messages possible (useful for audio-related bugs) | Archive is hard to search and analyse |
| Easy to share screenshots / short recordings | Group can become a support channel drain |
| Free, instant | Invite link can be shared if tester forwards it |

**Verdict**: Use as a supplement for the ~10 most engaged testers (those who
opted in via Form). Not a replacement for structured intake.

---

### Option D — GitHub Discussions (private repo)

**How it works**: Enable Discussions on the private repo. Invite
code-aware contributors (developers who you trust with repo access).

| Pros | Cons |
|---|---|
| Structured categories (Bug, Idea, Q&A, etc.) | Limited to people with repo invite — not scalable for 30 testers |
| Searchable, threaded | Requires GitHub account + explicit repo invite per person |
| Stays private | Manual overhead of managing 30 invites |

**Verdict**: Use for the 2-3 developer friends who contribute code-level
feedback. Not the primary channel.

---

## Recommendation

**Use a three-tier system: Form (primary) + Telegram (engaged testers) + GitHub Discussions (developers).**

```
All 20-30 testers
       │
       ▼
 [Google Form]  ← Primary inbound for all bug reports + feature requests
       │
       ├── Opt-in checkbox: "Tham gia Telegram group"
       │         │
       │         ▼
       │   [Telegram group]  ← ~10 most active testers, real-time loop
       │
       └── (future) Code-contributing developers
                 │
                 ▼
         [GitHub Discussions]  ← Invite-only, 2-3 devs max in Sprint 1
```

**Confidence level**: High. This pattern matches the project's constraints:
solo dev, private repo, Vietnamese-first audience, zero infrastructure budget.

---

## Setup Instructions

### Google Form — Set Up in 15 Minutes

1. Go to https://forms.google.com → click "+" to create new form.
2. Title: `Vọng AI Recorder — Phản hồi beta`
3. Description: `Cảm ơn bạn đã tham gia beta! Mọi phản hồi đều được đọc và trả lời trong vòng 48 giờ.`
4. Add fields in this order:

| Field | Type | Required |
|---|---|---|
| Tên của bạn | Short answer | No |
| Email hoặc Telegram username | Short answer | No |
| Loại phản hồi | Dropdown: `Bug · Góp ý tính năng · Câu hỏi · Khác` | Yes |
| Hệ điều hành | Dropdown: `Windows 11 · Windows 10 · Khác` | Yes |
| Mô tả chi tiết | Paragraph | Yes |
| Ảnh chụp màn hình (nếu có) | File upload (image only, max 10 MB) | No |
| Đăng ký Telegram group (nhóm thảo luận nhanh hơn) | Checkbox: `Có, tôi muốn tham gia` | No |

5. Settings → Responses → turn on "Collect email addresses" = Off (lowers friction).
6. Settings → Responses → turn on "Limit to 1 response" = Off (testers may submit multiple bugs).
7. Click "Send" → link icon → copy short URL (e.g. `forms.gle/XXXXX`).
8. Paste the URL into:
   - `app/vong-app/ui/app-window.slint` → About section link
   - `vong-landing/index.html` → footer beta section
   - Telegram group pinned message

### Google Form — Weekly Triage Workflow

Every Monday morning (15 minutes):

1. Open Form → Responses → View in Sheets.
2. Add columns: `Status` (New / In Progress / Fixed / Wont Fix), `Priority` (P0/P1/P2), `Sprint`.
3. Filter by "Loại phản hồi = Bug" → triage P0 first (crash / data loss / silent failure).
4. Respond to each reporter individually if they left contact info.
5. Copy P0/P1 bugs into your task list for the sprint.

### Telegram Group — Setup

1. Open Telegram → New Group → name: `Vọng AI Recorder — Beta Testers`.
2. Set group type: Private (invite link only).
3. Generate invite link: Group Settings → Invite via link → set expiry 30 days, limit 50 members.
4. Enable "Approve new members" — manually approve after cross-checking Form responses.
5. Pin a welcome message (template below).

---

## Template Messages

### Google Form — Confirmation Message (shown after submit)

```
Cảm ơn bạn rất nhiều! 🙏
Mình đọc mọi phản hồi trong vòng 24-48 giờ.

Nếu bạn đã chọn tham gia Telegram group, mình sẽ gửi link riêng cho bạn sớm nhé.

Trong lúc chờ, nếu bạn muốn theo dõi tiến độ phát triển:
→ https://vong.app/
```

### Telegram Group — Welcome Pinned Message

```
👋 Chào mừng đến với Vọng AI Recorder Beta Testers!

Nhóm này dành cho ~20-30 người dùng beta đầu tiên của Vọng.
Mình (tác giả) đọc tất cả tin nhắn trong nhóm.

📋 Quy tắc:
1. Bug có ảnh chụp màn hình → báo qua Google Form trước, rồi paste link ở đây
   Form: [FORM_URL]
2. Câu hỏi nhanh, thảo luận → chat trực tiếp trong nhóm
3. Không spam, không quảng cáo

🐛 Template báo bug nhanh:
---
Vấn đề: [mô tả ngắn]
Bước tái hiện: [1. ... 2. ... 3. ...]
Kết quả mong đợi: [...]
Kết quả thực tế: [...]
Windows version: [Win 10/11]
---

Cảm ơn các bạn đã bỏ thời gian test! Mọi góp ý đều có giá trị 🙏
```

### Weekly Digest Template (post to Telegram every Monday)

```
📊 Cập nhật tuần [N] — Vọng Beta

✅ Đã sửa: [list bug fixes]
🔧 Đang làm: [in-progress items]
📝 Ghi nhận: [list acknowledged issues not yet fixed]
❓ Cần thêm thông tin: [list issues needing reproduction steps]

Phiên bản mới (nếu có): v0.1.0-beta.X — tải tại https://vong.app/
```

---

## Migration Plan

If beta exceeds 30 testers and Telegram becomes unmanageable:

1. **< 30 testers**: current setup (Form + Telegram) is sufficient.
2. **30-100 testers**: add a second Telegram group (overflow), or switch to
   Discord with #bug-reports and #general channels.
3. **> 100 testers / repo goes public**: migrate to GitHub Issues. Set up
   issue templates for Bug Report / Feature Request / Question. Close the
   Google Form and redirect to Issues.

Estimated trigger for public repo decision: Sprint 2 review (Day 14 post-launch).
Hard requirement: AGPL commit history reviewed, brand trademark registered (or
accepted risk), landing page updated with "source available" badge.

---

## Summary

| Channel | Audience | Purpose | Priority |
|---|---|---|---|
| Google Form | All 20-30 testers | Structured bug intake, feature requests | Primary |
| Telegram group | ~10 opted-in testers | Real-time loop, fast debugging | Secondary |
| GitHub Discussions | 2-3 developer friends | Code-level feedback | Tertiary |
| GitHub Issues (public) | Future — Sprint 2+ | Public tracker after repo goes public | Deferred |
