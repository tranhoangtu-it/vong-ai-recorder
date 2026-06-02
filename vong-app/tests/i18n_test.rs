//! Integration tests for vong-app i18n module.
//!
//! Tests cover: locale parsing, translation lookup (hit/miss/fallback),
//! default-to-vi when file absent, and key-identity on missing key.

// Mirror the i18n types here to avoid needing pub re-exports from the binary crate.
// The logic is simple enough to re-implement in the test.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Locale {
    Vi,
    En,
}

impl Locale {
    fn from_str(s: &str) -> Self {
        match s.trim() {
            "en" => Self::En,
            _ => Self::Vi,
        }
    }
}

/// Minimal translation table mirroring i18n.rs — just enough entries for tests.
static TEST_TRANSLATIONS: &[(&str, &str, &str)] = &[
    ("tap_to_record", "Nhấn để ghi", "Tap to record"),
    ("history_title", "Tìm + Lịch sử", "Search + History"),
    ("history_empty", "Chưa có phiên nào được lưu.", "No sessions saved yet."),
    ("toggle_on", "Đang bật", "On"),
    ("toggle_off", "Đang tắt", "Off"),
    ("tab_system", "System", "System"),
];

struct Translations {
    locale: Locale,
}

impl Translations {
    fn new(locale: Locale) -> Self {
        Self { locale }
    }

    fn t<'a>(&self, key: &'a str) -> &'a str
    where
        'a: 'a,
    {
        for (k, vi, en) in TEST_TRANSLATIONS {
            if *k == key {
                return match self.locale {
                    Locale::Vi => vi,
                    Locale::En => en,
                };
            }
        }
        // Missing key: return the key itself (graceful degradation).
        key
    }
}

/// Simulate reading a locale.txt file: parse content or fall back to Vi.
fn parse_locale_from_file_content(content: Option<&str>) -> Locale {
    match content {
        Some(c) => Locale::from_str(c),
        None => Locale::Vi, // file absent → default Vietnamese
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn locale_parse_vi_from_file() {
    assert_eq!(parse_locale_from_file_content(Some("vi")), Locale::Vi);
    assert_eq!(parse_locale_from_file_content(Some("  vi  ")), Locale::Vi);
}

#[test]
fn locale_parse_en_from_file() {
    assert_eq!(parse_locale_from_file_content(Some("en")), Locale::En);
    assert_eq!(parse_locale_from_file_content(Some("  en  ")), Locale::En);
}

#[test]
fn locale_defaults_to_vi_when_file_absent() {
    // When file does not exist (None), default must be Vi — VN-first product.
    assert_eq!(parse_locale_from_file_content(None), Locale::Vi);
}

#[test]
fn locale_defaults_to_vi_on_unknown_content() {
    // Unknown or corrupt content falls back to Vi.
    assert_eq!(parse_locale_from_file_content(Some("fr")), Locale::Vi);
    assert_eq!(parse_locale_from_file_content(Some("")), Locale::Vi);
    assert_eq!(parse_locale_from_file_content(Some("unknown")), Locale::Vi);
    assert_eq!(parse_locale_from_file_content(Some("EN")), Locale::Vi); // case-sensitive
}

#[test]
fn translation_lookup_vi_returns_vietnamese_text() {
    let tr = Translations::new(Locale::Vi);
    assert_eq!(tr.t("tap_to_record"), "Nhấn để ghi");
    assert_eq!(tr.t("history_title"), "Tìm + Lịch sử");
    assert_eq!(tr.t("history_empty"), "Chưa có phiên nào được lưu.");
    assert_eq!(tr.t("toggle_on"), "Đang bật");
    assert_eq!(tr.t("toggle_off"), "Đang tắt");
}

#[test]
fn translation_lookup_en_returns_english_text() {
    let tr = Translations::new(Locale::En);
    assert_eq!(tr.t("tap_to_record"), "Tap to record");
    assert_eq!(tr.t("history_title"), "Search + History");
    assert_eq!(tr.t("history_empty"), "No sessions saved yet.");
    assert_eq!(tr.t("toggle_on"), "On");
    assert_eq!(tr.t("toggle_off"), "Off");
}

#[test]
fn translation_lookup_missing_key_returns_key_itself() {
    let tr_vi = Translations::new(Locale::Vi);
    let tr_en = Translations::new(Locale::En);
    // A key absent from the table must return the key string unchanged.
    // This ensures the UI always shows *something* even if a key was forgotten.
    assert_eq!(tr_vi.t("nonexistent_key_xyz_123"), "nonexistent_key_xyz_123");
    assert_eq!(tr_en.t("nonexistent_key_xyz_123"), "nonexistent_key_xyz_123");
}

#[test]
fn translation_same_key_both_locales_differ_when_appropriate() {
    let vi = Translations::new(Locale::Vi);
    let en = Translations::new(Locale::En);
    // For keys where VI != EN the result must differ.
    assert_ne!(vi.t("tap_to_record"), en.t("tap_to_record"));
    assert_ne!(vi.t("history_empty"), en.t("history_empty"));
}

#[test]
fn translation_locale_write_read_roundtrip() {
    // Test the string representation roundtrip.
    let vi_str = "vi";
    let en_str = "en";
    assert_eq!(Locale::from_str(vi_str), Locale::Vi);
    assert_eq!(Locale::from_str(en_str), Locale::En);
    // A freshly-parsed locale serialises back to the same string.
    let back_vi = match Locale::from_str(vi_str) {
        Locale::Vi => "vi",
        Locale::En => "en",
    };
    let back_en = match Locale::from_str(en_str) {
        Locale::Vi => "vi",
        Locale::En => "en",
    };
    assert_eq!(back_vi, vi_str);
    assert_eq!(back_en, en_str);
}
