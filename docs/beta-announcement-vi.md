# Bản thảo thông báo beta — Tiếng Việt

Dùng cho: Facebook (bài đăng cá nhân + group), Voz F17, blog cá nhân, dev.to (bản VI).
Độ dài: ~500 từ. Tone: thành thật, kỹ thuật nhưng gần gũi — không hype.

---

## Bài đăng Facebook / Voz (bản đầy đủ)

---

**🎙 Vọng AI Recorder — bản beta đầu tiên đã sẵn sàng**

Mình có một thói quen khá phổ biến với dân làm remote: họp bằng tiếng Anh, ghi chú bằng tiếng Việt, rồi mất 10-15 phút sau mỗi cuộc họp để tổng hợp lại những gì vừa được nói. Khi số cuộc họp tăng lên, cái 15 phút đó bắt đầu thành vài tiếng mỗi tuần.

Mình thử dùng Otter.ai, Fireflies — đều tốt, nhưng audio phải gửi lên server của họ. Với những cuộc họp có nội dung nhạy cảm (khách hàng, chiến lược sản phẩm), điều đó không phải lúc nào cũng ổn.

Vì vậy mình tự build một cái.

---

**Vọng AI Recorder** là ứng dụng desktop Windows ghi âm mic hoặc loa hệ thống (loopback), phiên âm và dịch nói đa ngôn ngữ — **chạy hoàn toàn trên máy, không cần Internet**.

✨ **Tính năng chính:**

- **Whisper offline, miễn phí** — mô hình base (~148 MB) chạy CPU. Không subscription, không cloud, không tài khoản. GPU Vulkan tùy chọn: nhanh hơn CPU 35-100 lần.
- **Streaming "nói đến đâu hiện đến đấy"** — văn bản xuất hiện trong khoảng 1-2 giây, không cần đợi hết câu.
- **Dịch sang tiếng Anh tự động** — hai cột song song: bản gốc (trái) và bản dịch (phải). Nhận diện ngôn ngữ tự động.
- **Tìm kiếm giọng điệu** — gõ `khong` tìm được `không`, `xin chao` tìm được `xin chào`. Hỗ trợ FTS5 tone-insensitive cho tiếng Việt.
- **Lưu trữ toàn bộ lịch sử** — mỗi phiên ghi âm được lưu vào SQLite local. Xuất ra Markdown bất cứ lúc nào.
- **BYOK cloud nếu cần tốc độ cao hơn** — Soniox hoặc OpenAI Realtime, API key lưu trong Windows Credential Manager (không lưu text thuần).

🔐 **Quyền riêng tư là cốt lõi:**
Audio không bao giờ rời khỏi máy bạn khi dùng Whisper Local. Mã nguồn AGPL-3.0, mở hoàn toàn. Không telemetry.

---

**🚀 Tải về:**
https://vong.app/
Yêu cầu: Windows 10/11 64-bit · ~12 MB (CPU) hoặc ~65 MB (Vulkan GPU)

Cách cài:
1. Tải file `.msi` từ trang web
2. Windows SmartScreen sẽ hiện cảnh báo "Unknown publisher" — đây là bình thường với phần mềm chưa có chứng chỉ ký số. Nhấn "More info" → "Run anyway"
3. Cài đặt không cần quyền admin (per-user install)

---

**🐛 Mình đang tìm 20-30 người dùng beta**

Nếu bạn:
- Thường xuyên họp online với đồng nghiệp hoặc khách hàng nói tiếng Anh / tiếng nước ngoài
- Dùng Windows 10 hoặc 11
- Sẵn sàng bỏ 15-30 phút thử và cho mình biết cảm nhận thật

→ Đăng ký tại: **[FEEDBACK_FORM_URL]**

Những người đăng ký sẽ nhận được link tham gia nhóm Telegram riêng để thảo luận trực tiếp với mình trong quá trình beta.

---

**📖 Mã nguồn:**
github.com/tranhoangtu-it/vong-ai-recorder (hiện private, sẽ mở sau khi beta ổn định)
Giấy phép: AGPL-3.0

Mình build cái này trong thời gian rảnh, một mình, hoàn toàn bằng Rust + Slint. Mọi góp ý đều có giá trị — dù chỉ là "cài xong không biết bấm gì" cũng là thông tin hữu ích.

Cảm ơn cộng đồng đã đọc đến đây 🙏

#VongAIRecorder #SpeechToText #VietnamTech #RustLang #OpenSource

---

## Bản rút gọn — Twitter / Voz (dưới 280 ký tự)

```
Vọng AI Recorder beta — ghi âm + dịch nói offline trên Windows.
Whisper chạy local, không cloud, không subscription.
Tìm 20-30 beta tester người Việt.
Tải: https://vong.app/
Đăng ký test: [FORM_URL]
#VongAIRecorder #VietnamTech
```

---

## Ghi chú cho người đăng bài

- Thay `[FEEDBACK_FORM_URL]` và `[FORM_URL]` bằng link Google Form thực sau khi tạo.
- Nếu đã có screenshot thực của app, đính kèm 1-2 ảnh vào bài Facebook để tăng reach.
- Đăng vào khung giờ 7-9 giờ tối hoặc 12-1 giờ trưa (giờ Việt Nam) để đạt tương tác cao nhất.
- Trả lời mọi bình luận trong 24 giờ đầu — Facebook ưu tiên bài có tương tác sớm.
- Với Voz: đăng trong subforum Hardware & Software, tiêu đề thread nên rõ ràng không clickbait.
