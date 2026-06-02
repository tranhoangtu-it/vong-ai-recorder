//! Lean i18n module for Vọng AI Recorder.
//!
//! Supports two locales: Vietnamese (default, VN-first product) and English.
//! Uses a compile-time lookup table — no gettext/.po files needed.
//! Persisted to `%APPDATA%\Vong\Vong AI Recorder\config\locale.txt`.

use std::collections::HashMap;
use std::path::PathBuf;

/// The two supported UI locales.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    Vi,
    En,
}

impl Locale {
    /// Parse from a locale.txt content string. Falls back to `Vi` on unknown input.
    pub fn from_str(s: &str) -> Self {
        match s.trim() {
            "en" => Self::En,
            _ => Self::Vi,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vi => "vi",
            Self::En => "en",
        }
    }
}

/// Compile-time translation table: key → (Vietnamese, English).
/// Keys are stable identifiers — never reference plan artifacts.
static TRANSLATIONS: &[(&str, &str, &str)] = &[
    // ── Main view ───────────────────────────────────────────────────────────
    ("tap_to_record",          "Nhấn để ghi",                    "Tap to record"),
    ("recording_active",       "● Đang ghi…  (nhấn để dừng)",   "● Recording…  (tap to stop)"),
    ("transcript_original",    "Bản gốc",                        "Original"),
    ("transcript_translated",  "Bản dịch",                       "Translation"),
    ("waiting_speech",         "Đang chờ giọng nói…",            "Waiting for speech…"),
    ("col_hint_original",      "Whisper auto-detect sẽ điền cột này ngay\nkhi mỗi utterance được nhận diện.",
                               "Whisper auto-detect fills this column\nwhen each utterance is recognized."),
    ("col_hint_translated",    "Whisper với gợi ý tiếng Việt điền cột này\nsau khi pass đầu hoàn tất.",
                               "Whisper with language hint fills this column\nafter the first pass completes."),
    ("silence_placeholder",    "(im lặng)",                      "(silence)"),
    ("no_translation",         "(không có)",                     "(none)"),
    ("translating",            "⏳  đang dịch…",                 "⏳  translating…"),
    // ── History / sidebar ────────────────────────────────────────────────────
    ("history_title",          "Tìm + Lịch sử",                  "Search + History"),
    ("history_empty",          "Chưa có phiên nào được lưu.",    "No sessions saved yet."),
    ("search_placeholder",     "Tìm trong tất cả phiên ghi…",    "Search all sessions…"),
    ("export_btn",             "📥  Xuất phiên mới nhất → Markdown", "📥  Export latest session → Markdown"),
    // ── Settings tab names ────────────────────────────────────────────────────
    ("tab_system",             "System",                          "System"),
    ("tab_voice_typing",       "Voice Typing",                    "Voice Typing"),
    ("tab_recording",          "Recording",                       "Recording"),
    ("tab_language",           "Language",                        "Language"),
    ("tab_dictionary",         "Dictionary",                      "Dictionary"),
    ("tab_notification",       "Notification",                    "Notification"),
    // ── Settings section titles ───────────────────────────────────────────────
    ("settings_title",         "Cài đặt",                        "Settings"),
    ("section_system",         "System",                          "System"),
    ("section_system_sub",     "STT engine + thông tin ứng dụng", "STT engine + app info"),
    ("section_recording",      "Recording",                       "Recording"),
    ("section_recording_sub",  "Nguồn âm thanh, VAD, mô hình Whisper", "Audio source, VAD, Whisper model"),
    ("section_language",       "Language",                        "Language"),
    ("section_language_sub",   "Ngôn ngữ nguồn (Bản gốc) + đích (Bản dịch)",
                               "Source language (Original) + target (Translation)"),
    ("section_voice_typing",   "Voice Typing",                    "Voice Typing"),
    ("section_voice_typing_sub","Nhấn tổ hợp phím bất cứ đâu để nói, văn bản sẽ tự động gõ vào ứng dụng đang focus",
                               "Press the hotkey anywhere to speak; text is automatically typed into the focused app"),
    // ── Settings card titles ──────────────────────────────────────────────────
    ("stt_engine",             "STT engine",                      "STT engine"),
    ("audio_source",           "Nguồn âm thanh",                  "Audio source"),
    ("crash_reporting",        "Báo cáo lỗi (Sentry)",           "Error reporting (Sentry)"),
    ("auto_update",            "Cập nhật tự động",               "Auto update"),
    ("auto_summary",           "Tóm tắt phiên (AI)",             "Session summary (AI)"),
    ("diarization",            "Nhận diện người nói (chỉ Soniox)", "Speaker detection (Soniox only)"),
    ("whisper_model",          "Mô hình Whisper",                 "Whisper model"),
    ("dictionary_title",       "Từ điển tùy chỉnh",              "Custom dictionary"),
    ("language_card",          "Ngôn ngữ",                        "Language"),
    // ── Interface language card ────────────────────────────────────────────────
    ("interface_language",     "Ngôn ngữ giao diện / Interface language", "Interface language / Ngôn ngữ giao diện"),
    ("locale_vi",              "Tiếng Việt",                      "Tiếng Việt"),
    ("locale_en",              "English",                         "English"),
    // ── SessionDetail ────────────────────────────────────────────────────────
    ("session_summary",        "Tóm tắt phiên",                   "Session summary"),
    ("session_content",        "Nội dung phiên",                  "Session content"),
    ("session_no_segments",    "Chưa có đoạn nào được ghi.",      "No segments recorded yet."),
    ("summary_idle",           "Chưa có tóm tắt. Bật tự động tóm tắt hoặc nhấn Tạo lại.",
                               "No summary yet. Enable auto-summary or click Regenerate."),
    ("summary_computing",      "Đang tạo tóm tắt…",              "Generating summary…"),
    ("summary_needs_key",      "Cần API key OpenAI — cấu hình trong Cài đặt → System.",
                               "OpenAI API key required — configure in Settings → System."),
    ("btn_regenerate",         "↺ Tạo lại",                       "↺ Regenerate"),
    ("btn_copy",               "📋 Sao chép",                     "📋 Copy"),
    ("placeholder_coming",     "Tính năng sẽ có ở bản kế tiếp.", "Feature coming in a future release."),
    // ── Restart / save hints ──────────────────────────────────────────────────
    ("restart_to_apply",       "↻  Đã lưu — khởi động lại Vọng AI Recorder để áp dụng",
                               "↻  Saved — restart Vọng AI Recorder to apply"),
    // ── Toggle states ─────────────────────────────────────────────────────────
    ("toggle_on",              "Đang bật",                        "On"),
    ("toggle_off",             "Đang tắt",                        "Off"),
    // ── Common actions ────────────────────────────────────────────────────────
    ("btn_save",               "💾  Lưu key",                     "💾  Save key"),
    ("btn_check_now",          "Kiểm tra ngay",                   "Check now"),
    ("btn_add",                "+ Thêm",                          "+ Add"),
    ("api_key_label",          "API key (lưu vào Keychain):",     "API key (saved to Keychain):"),
];

/// A simple translation lookup table keyed by stable string identifiers.
pub struct Translations {
    locale: Locale,
    table: HashMap<&'static str, (&'static str, &'static str)>,
}

impl Translations {
    pub fn new(locale: Locale) -> Self {
        let table = TRANSLATIONS
            .iter()
            .map(|(k, vi, en)| (*k, (*vi, *en)))
            .collect();
        Self { locale, table }
    }

    /// Look up a translation. Returns the key itself when the key is absent
    /// (graceful degradation — never panics, shows something meaningful).
    pub fn t<'a>(&self, key: &'a str) -> &'a str {
        if let Some((vi, en)) = self.table.get(key) {
            match self.locale {
                Locale::Vi => vi,
                Locale::En => en,
            }
        } else {
            // Key not found — return key itself so UI shows something debuggable.
            // Safe: key is &'static str used only for lookup; no user content.
            key
        }
    }

    // Used in tests to verify locale after construction.
    #[allow(dead_code)]
    pub fn locale(&self) -> Locale {
        self.locale
    }

    // Used in tests to switch locale on an existing Translations instance.
    #[allow(dead_code)]
    pub fn with_locale(mut self, locale: Locale) -> Self {
        self.locale = locale;
        self
    }
}

// ── Persistence helpers ───────────────────────────────────────────────────────

/// `%APPDATA%\Vong\Vong AI Recorder\config\locale.txt`
pub fn locale_config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "Vong", "Vong AI Recorder")
        .map(|d| d.config_dir().join("locale.txt"))
}

/// Load locale from disk. Falls back to `Vi` when the file is absent or unreadable.
/// Default is Vietnamese — VN-first product; user must explicitly choose EN.
pub fn load_locale() -> Locale {
    if let Some(path) = locale_config_path() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            return Locale::from_str(&content);
        }
    }
    Locale::Vi
}

/// Persist the locale selection. Creates the config directory if needed.
pub fn save_locale(locale: Locale) -> std::io::Result<()> {
    let Some(path) = locale_config_path() else {
        return Err(std::io::Error::other("ProjectDirs unavailable"));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, locale.as_str())
}

// ── Last-session persistence helpers ─────────────────────────────────────────

/// `%APPDATA%\Vong\Vong AI Recorder\config\last_session.txt`
pub fn last_session_config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "Vong", "Vong AI Recorder")
        .map(|d| d.config_dir().join("last_session.txt"))
}

/// Write the last-viewed session id. Creates the config directory if needed.
pub fn save_last_session(session_id: i64) -> std::io::Result<()> {
    let Some(path) = last_session_config_path() else {
        return Err(std::io::Error::other("ProjectDirs unavailable"));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, session_id.to_string())
}

/// Read the last-viewed session id. Returns `None` when the file is absent,
/// empty, or contains a non-integer value.
pub fn load_last_session() -> Option<i64> {
    let path = last_session_config_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    content.trim().parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_parse_vi_from_string() {
        assert_eq!(Locale::from_str("vi"), Locale::Vi);
        assert_eq!(Locale::from_str("  vi  "), Locale::Vi);
    }

    #[test]
    fn locale_parse_en_from_string() {
        assert_eq!(Locale::from_str("en"), Locale::En);
        assert_eq!(Locale::from_str("  en  "), Locale::En);
    }

    #[test]
    fn locale_parse_defaults_to_vi_on_unknown() {
        assert_eq!(Locale::from_str("fr"), Locale::Vi);
        assert_eq!(Locale::from_str(""), Locale::Vi);
        assert_eq!(Locale::from_str("unknown"), Locale::Vi);
    }

    #[test]
    fn translation_lookup_vi() {
        let tr = Translations::new(Locale::Vi);
        assert_eq!(tr.t("tap_to_record"), "Nhấn để ghi");
        assert_eq!(tr.t("history_title"), "Tìm + Lịch sử");
        assert_eq!(tr.t("tab_system"), "System");
    }

    #[test]
    fn translation_lookup_en() {
        let tr = Translations::new(Locale::En);
        assert_eq!(tr.t("tap_to_record"), "Tap to record");
        assert_eq!(tr.t("history_title"), "Search + History");
        assert_eq!(tr.t("history_empty"), "No sessions saved yet.");
    }

    #[test]
    fn translation_lookup_missing_key_returns_key() {
        let tr_vi = Translations::new(Locale::Vi);
        let tr_en = Translations::new(Locale::En);
        // A key not in the table returns the key itself — never panics.
        assert_eq!(tr_vi.t("nonexistent_key_xyz"), "nonexistent_key_xyz");
        assert_eq!(tr_en.t("nonexistent_key_xyz"), "nonexistent_key_xyz");
    }

    #[test]
    fn translation_with_locale_switch() {
        let tr = Translations::new(Locale::Vi);
        assert_eq!(tr.t("tap_to_record"), "Nhấn để ghi");
        let tr_en = tr.with_locale(Locale::En);
        assert_eq!(tr_en.t("tap_to_record"), "Tap to record");
    }
}
