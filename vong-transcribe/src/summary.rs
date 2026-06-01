//! OpenAI chat-completions summary call for session transcripts.
//!
//! Truncates long transcripts (> 80 000 chars) keeping head + tail.
//! Retries once on 429 (honoring `Retry-After`, capped 30 s) or 5xx (3 s backoff).
//!
//! Privacy rule: NEVER log transcript text, prompt content, summary text, or API key.
//! Only metadata (char counts, token counts, latency, error codes) is logged.

use std::time::Duration;

/// Successfully parsed OpenAI response.
#[derive(Debug, Clone)]
pub struct SummaryResponse {
    /// Summary text (Vietnamese, 3-5 sentences).
    pub content: String,
    /// Dated model variant from the response e.g. `"gpt-4o-mini-2024-07-18"`.
    pub model: String,
    /// Prompt token count (for cost auditing via logs).
    pub prompt_tokens: u64,
    /// Completion token count.
    pub completion_tokens: u64,
}

/// Errors that can occur during summary generation.
#[derive(Debug, thiserror::Error)]
pub enum SummaryError {
    /// API key absent in keychain — user needs to configure one.
    #[error("API key not found — configure OpenAI key in Settings → System")]
    NeedsApiKey,

    /// API key rejected by OpenAI (401).
    #[error("API key không hợp lệ — kiểm tra Settings → System")]
    InvalidKey,

    /// Rate limited (429) and retry also failed.
    #[error("Hết hạn mức OpenAI — thử lại sau")]
    RateLimited,

    /// Server error (5xx) even after retry.
    #[error("Lỗi server OpenAI ({0}) — thử lại sau")]
    ServerError(u16),

    /// Request timed out (30 s).
    #[error("Hết thời gian chờ — kiểm tra kết nối mạng")]
    Timeout,

    /// Any other error (non-2xx status or parse failure).
    #[error("Lỗi provider (code={code}): {message}")]
    Provider {
        /// HTTP status code (0 for network/transport errors).
        code: u16,
        /// Error description (never contains user data — see privacy rule).
        message: String,
    },
}

/// Truncate `text` to at most `max_chars` characters, keeping the first half
/// and last half with an elision marker in the middle.
///
/// Slices are adjusted to char boundaries to prevent UTF-8 panics.
pub fn truncate_transcript(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    // Reserve ~100 chars for the elision marker; split the rest equally.
    let half = max_chars.saturating_sub(100) / 2;

    // Find byte offset for the prefix (first `half` chars).
    let prefix_end = text
        .char_indices()
        .nth(half)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    let prefix = &text[..prefix_end];

    // Find byte offset for the suffix start (last `half` chars).
    let total_chars = text.chars().count();
    let suffix_char_start = total_chars.saturating_sub(half);
    let suffix_start = text
        .char_indices()
        .nth(suffix_char_start)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    let suffix = &text[suffix_start..];

    format!("{prefix}\n\n… [đã bỏ qua phần giữa] …\n\n{suffix}")
}

/// Build the system prompt for the summary call (static, Vietnamese).
fn build_system_prompt() -> &'static str {
    "Bạn là trợ lý tóm tắt cuộc nói chuyện bằng tiếng Việt, ngắn gọn 3-5 câu. \
     Nêu chủ đề chính, các quyết định / hành động cụ thể nếu có. Không bịa thông tin."
}

/// Call OpenAI chat completions to summarize `transcript`.
///
/// Retries once on 429 (honors `Retry-After` header, capped 30 s) and once
/// on 5xx (3 s fixed backoff). Returns `SummaryError::Timeout` if the
/// overall HTTP call exceeds 30 s.
pub async fn generate_summary(
    api_key: &crate::ApiKey,
    transcript: &str,
) -> Result<SummaryResponse, SummaryError> {
    let trimmed = truncate_transcript(transcript, 80_000);

    let body = serde_json::json!({
        "model": "gpt-4o-mini",
        "messages": [
            { "role": "system", "content": build_system_prompt() },
            { "role": "user",   "content": trimmed }
        ],
        "max_tokens": 500,
        "temperature": 0.3
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| SummaryError::Provider {
            code: 0,
            message: e.to_string(),
        })?;

    let mut last_error: Option<SummaryError> = None;

    for attempt in 0..2u8 {
        let resp = client
            .post("https://api.openai.com/v1/chat/completions")
            .bearer_auth(api_key.expose())
            .json(&body)
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                let err = if e.is_timeout() {
                    SummaryError::Timeout
                } else {
                    SummaryError::Provider {
                        code: 0,
                        message: e.to_string(),
                    }
                };
                last_error = Some(err);
                if attempt == 0 {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue;
                }
                break;
            }
        };

        let status = resp.status().as_u16();
        match status {
            200 => {
                let parsed: serde_json::Value =
                    resp.json().await.map_err(|e| SummaryError::Provider {
                        code: 200,
                        message: format!("parse: {e}"),
                    })?;
                let content = parsed["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if content.is_empty() {
                    return Err(SummaryError::Provider {
                        code: 200,
                        message: "empty choices content".into(),
                    });
                }
                let model = parsed["model"]
                    .as_str()
                    .unwrap_or("gpt-4o-mini")
                    .to_string();
                let prompt_tokens =
                    parsed["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
                let completion_tokens =
                    parsed["usage"]["completion_tokens"].as_u64().unwrap_or(0);
                return Ok(SummaryResponse {
                    content,
                    model,
                    prompt_tokens,
                    completion_tokens,
                });
            }
            401 => return Err(SummaryError::InvalidKey),
            429 => {
                if attempt == 0 {
                    let wait_secs = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(5)
                        .min(30);
                    tokio::time::sleep(Duration::from_secs(wait_secs)).await;
                    last_error = Some(SummaryError::RateLimited);
                    continue;
                }
                return Err(SummaryError::RateLimited);
            }
            500..=599 => {
                last_error = Some(SummaryError::ServerError(status));
                if attempt == 0 {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue;
                }
                return Err(SummaryError::ServerError(status));
            }
            other => {
                // Consume body text for the error message but do NOT log it
                // (may contain user-identifiable info in error payloads).
                let _body = resp.text().await.unwrap_or_default();
                return Err(SummaryError::Provider {
                    code: other,
                    message: format!("unexpected status {other}"),
                });
            }
        }
    }

    Err(last_error.unwrap_or(SummaryError::Timeout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_passes_short_input_through() {
        let s = truncate_transcript("hello", 1000);
        assert_eq!(s, "hello");
    }

    #[test]
    fn truncate_long_input_keeps_head_and_tail() {
        let long: String = "x".repeat(200_000);
        let out = truncate_transcript(&long, 80_000);
        // Slop: marker adds ~60 chars; total should be close to max_chars.
        assert!(out.len() <= 81_000, "truncated output too long: {}", out.len());
        assert!(out.starts_with("xxxx"));
        assert!(out.ends_with("xxxx"));
        assert!(out.contains("[đã bỏ qua phần giữa]"));
    }

    #[test]
    fn truncate_respects_char_boundary() {
        // Each "đ" = 2 bytes in UTF-8. Slice must stay on char boundary.
        let viet: String = "đ".repeat(50_000);
        let out = truncate_transcript(&viet, 80_000);
        // If we sliced mid-char this would panic; just validate it's valid UTF-8.
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        assert!(out.is_char_boundary(out.len()));
    }

    #[test]
    fn truncate_marker_in_middle() {
        // 200k 'a' chars; verify the marker appears.
        let long: String = "a".repeat(200_000);
        let out = truncate_transcript(&long, 1_000);
        assert!(out.contains("… [đã bỏ qua phần giữa] …"));
        // Must start with 'a' and end with 'a'.
        assert!(out.starts_with('a'));
        assert!(out.ends_with('a'));
    }

    #[test]
    fn truncate_exact_boundary_not_truncated() {
        let s: String = "a".repeat(80_000);
        let out = truncate_transcript(&s, 80_000);
        assert_eq!(out, s);
    }
}
