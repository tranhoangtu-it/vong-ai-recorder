//! Toast notification infrastructure.
//!
//! `ToastQueue` is a shared, thread-safe queue of in-flight toast banners.
//! Each banner has a severity, message, expiry timestamp, and a unique id.
//! The Slint 30 Hz timer calls `prune_expired` + reads the Vec to mirror into
//! the UI; callers use `push` to enqueue and `dismiss` for explicit close.
//!
//! Privacy rule: messages must NEVER contain transcript content, audio data,
//! API keys, or user file paths. Use generic Vietnamese strings only.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{SystemTime, UNIX_EPOCH};

/// Severity level of a toast banner — drives background color in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastSeverity {
    Info,
    Warn,
    Error,
}

impl ToastSeverity {
    /// Returns a stable string token used by the Slint side for color routing.
    pub fn as_str(self) -> &'static str {
        match self {
            ToastSeverity::Info => "info",
            ToastSeverity::Warn => "warn",
            ToastSeverity::Error => "error",
        }
    }
}

/// A single in-flight toast entry.
#[derive(Debug, Clone)]
pub struct ToastEntry {
    /// Unique auto-incremented id — used by `dismiss`.
    pub id: u64,
    /// Display message (≤200 chars, Vietnamese, no sensitive data).
    pub message: String,
    /// Severity drives the background color chip.
    pub severity: ToastSeverity,
    /// Unix-epoch milliseconds after which this toast should be pruned.
    pub expires_at_ms: u64,
}

/// Shared, thread-safe toast queue. Clone the `Arc` to share across threads.
#[derive(Clone, Default)]
pub struct ToastQueue {
    inner: Arc<Mutex<Vec<ToastEntry>>>,
    next_id: Arc<AtomicU64>,
}

/// Auto-dismiss window in milliseconds for all severities.
const TOAST_DURATION_MS: u64 = 5_000;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl ToastQueue {
    /// Create a new empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a new toast. Returns the assigned id (for explicit dismiss later).
    ///
    /// The message is truncated to 200 chars to guard against accidentally
    /// surfacing large external error strings from APIs.
    pub fn push(&self, message: impl Into<String>, severity: ToastSeverity) -> u64 {
        let mut msg = message.into();
        // Privacy guard: cap length to avoid surfacing large external payloads.
        if msg.chars().count() > 200 {
            let truncated: String = msg.chars().take(197).collect();
            msg = format!("{truncated}…");
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let entry = ToastEntry {
            id,
            message: msg,
            severity,
            expires_at_ms: now_ms() + TOAST_DURATION_MS,
        };
        if let Ok(mut v) = self.inner.lock() {
            v.push(entry);
        }
        id
    }

    /// Remove a specific toast immediately (e.g. user clicked the ✕ button).
    pub fn dismiss(&self, id: u64) {
        if let Ok(mut v) = self.inner.lock() {
            v.retain(|e| e.id != id);
        }
    }

    /// Drop all entries whose `expires_at_ms` is in the past.
    /// Call this from the 30 Hz timer before reading the Vec.
    pub fn prune_expired(&self) {
        let now = now_ms();
        if let Ok(mut v) = self.inner.lock() {
            v.retain(|e| e.expires_at_ms > now);
        }
    }

    /// Read a snapshot of the current (non-expired) entries.
    /// Callers should call `prune_expired()` first for accurate results.
    pub fn snapshot(&self) -> Vec<ToastEntry> {
        self.inner
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Clear all toasts immediately (e.g. on app shutdown).
    #[allow(dead_code)]
    pub fn clear(&self) {
        if let Ok(mut v) = self.inner.lock() {
            v.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn push_increments_id() {
        let q = ToastQueue::new();
        let id0 = q.push("msg 0", ToastSeverity::Info);
        let id1 = q.push("msg 1", ToastSeverity::Warn);
        let id2 = q.push("msg 2", ToastSeverity::Error);
        assert!(id1 > id0);
        assert!(id2 > id1);
    }

    #[test]
    fn snapshot_contains_pushed_entries() {
        let q = ToastQueue::new();
        q.push("Đã sao chép tóm tắt", ToastSeverity::Info);
        q.push("Tải mô hình thất bại", ToastSeverity::Error);
        let snap = q.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].severity, ToastSeverity::Info);
        assert_eq!(snap[1].severity, ToastSeverity::Error);
    }

    #[test]
    fn prune_expired_removes_old_entries() {
        let q = ToastQueue::new();
        // Manually push an entry with an already-expired timestamp.
        {
            let id = q.next_id.fetch_add(1, Ordering::Relaxed);
            let entry = ToastEntry {
                id,
                message: "expired".into(),
                severity: ToastSeverity::Info,
                expires_at_ms: 1, // epoch ms 1 — already expired
            };
            q.inner.lock().unwrap().push(entry);
        }
        // Push a fresh one with a future expiry.
        q.push("still alive", ToastSeverity::Info);

        q.prune_expired();
        let snap = q.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].message, "still alive");
    }

    #[test]
    fn dismiss_by_id_removes_correct_entry() {
        let q = ToastQueue::new();
        let id_a = q.push("toast A", ToastSeverity::Info);
        let _id_b = q.push("toast B", ToastSeverity::Warn);
        q.dismiss(id_a);
        let snap = q.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].message, "toast B");
    }

    #[test]
    fn severity_as_str_matches_expected_tokens() {
        assert_eq!(ToastSeverity::Info.as_str(), "info");
        assert_eq!(ToastSeverity::Warn.as_str(), "warn");
        assert_eq!(ToastSeverity::Error.as_str(), "error");
    }

    #[test]
    fn message_truncated_to_200_chars() {
        let q = ToastQueue::new();
        let long_msg = "x".repeat(300);
        let id = q.push(long_msg, ToastSeverity::Error);
        let snap = q.snapshot();
        let entry = snap.iter().find(|e| e.id == id).unwrap();
        // 197 chars + "…" = 200 chars
        assert!(entry.message.chars().count() <= 200);
    }

    #[test]
    fn push_is_thread_safe() {
        let q = ToastQueue::new();
        let q2 = q.clone();
        let handle = thread::spawn(move || {
            q2.push("from thread", ToastSeverity::Info);
        });
        q.push("from main", ToastSeverity::Info);
        handle.join().unwrap();
        assert_eq!(q.snapshot().len(), 2);
    }
}
