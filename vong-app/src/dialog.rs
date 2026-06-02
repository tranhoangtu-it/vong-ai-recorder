//! Modal dialog infrastructure.
//!
//! `DialogQueue` manages a single at-a-time confirm dialog. Callers call
//! `confirm(title, body)` to show a modal dialog and await the user's
//! choice via a `tokio::sync::oneshot` channel. The Slint side fires
//! `dialog-confirmed` or `dialog-cancelled`, which resolves the oneshot.
//!
//! Only one dialog can be open at a time. If a second `confirm` call arrives
//! while one is already open the pending sender is dropped (auto-cancels it)
//! and the new dialog replaces it.

use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// Shared dialog state, pushed into Slint UI via the 30 Hz timer.
#[derive(Default)]
pub struct DialogState {
    /// Whether the modal overlay should be visible.
    pub open: bool,
    /// Dialog title (bold text at top of card).
    pub title: String,
    /// Dialog body message (multi-line descriptive text).
    pub body: String,
}

/// Inner mutable state protected by a single Mutex.
#[derive(Default)]
struct Inner {
    state: DialogState,
    /// Pending oneshot sender — set when dialog is open, cleared on resolution.
    pending_tx: Option<oneshot::Sender<bool>>,
}

/// Shared dialog controller. Clone the `Arc` to share across callbacks.
#[derive(Clone, Default)]
pub struct DialogQueue {
    inner: Arc<Mutex<Inner>>,
}

impl DialogQueue {
    /// Create a new dialog queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the dialog with the given title + body.
    ///
    /// Returns a `oneshot::Receiver<bool>` that resolves to:
    /// - `true`  when the user clicks "Đồng ý" (confirm)
    /// - `false` when the user clicks "Huỷ" (cancel) or presses Escape
    ///
    /// If a dialog is already open its pending sender is dropped (resolves to
    /// `Err(RecvError)` for the previous caller) and the new dialog replaces it.
    pub fn confirm(
        &self,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        if let Ok(mut g) = self.inner.lock() {
            // Drop any pending sender — previous dialog auto-cancelled.
            g.pending_tx = None;
            g.state = DialogState {
                open: true,
                title: title.into(),
                body: body.into(),
            };
            g.pending_tx = Some(tx);
        }
        rx
    }

    /// Resolve the open dialog as confirmed (`true`) and close it.
    /// Called from the Slint `dialog-confirmed` callback.
    pub fn resolve_confirmed(&self) {
        self.resolve(true);
    }

    /// Resolve the open dialog as cancelled (`false`) and close it.
    /// Called from the Slint `dialog-cancelled` callback.
    pub fn resolve_cancelled(&self) {
        self.resolve(false);
    }

    fn resolve(&self, choice: bool) {
        if let Ok(mut g) = self.inner.lock() {
            g.state.open = false;
            if let Some(tx) = g.pending_tx.take() {
                // Best-effort send — receiver may have been dropped if caller
                // timed out or was cancelled.
                let _ = tx.send(choice);
            }
        }
    }

    /// Snapshot the current dialog state for the Slint timer to push into the UI.
    ///
    /// Returns `(open, title, body)`.
    pub fn snapshot(&self) -> (bool, String, String) {
        self.inner
            .lock()
            .map(|g| {
                (
                    g.state.open,
                    g.state.title.clone(),
                    g.state.body.clone(),
                )
            })
            .unwrap_or((false, String::new(), String::new()))
    }

    /// True when a dialog is currently open (convenience accessor).
    #[allow(dead_code)]
    pub fn is_open(&self) -> bool {
        self.inner
            .lock()
            .map(|g| g.state.open)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn confirm_roundtrip_true() {
        let dq = DialogQueue::new();
        let rx = dq.confirm("Test title", "Test body");
        assert!(dq.is_open());
        dq.resolve_confirmed();
        assert!(!dq.is_open());
        let choice = rx.await.expect("receiver should not be dropped");
        assert!(choice);
    }

    #[tokio::test]
    async fn confirm_roundtrip_false() {
        let dq = DialogQueue::new();
        let rx = dq.confirm("Huỷ test", "Bạn có chắc không?");
        dq.resolve_cancelled();
        let choice = rx.await.expect("receiver should not be dropped");
        assert!(!choice);
    }

    #[tokio::test]
    async fn second_confirm_drops_first() {
        let dq = DialogQueue::new();
        let rx1 = dq.confirm("First", "First body");
        let rx2 = dq.confirm("Second", "Second body");

        // First receiver was dropped when second dialog opened.
        assert!(rx1.await.is_err(), "first dialog should be auto-cancelled");

        // Second dialog should resolve normally.
        dq.resolve_confirmed();
        let choice = rx2.await.unwrap();
        assert!(choice);
    }

    #[test]
    fn snapshot_reflects_state() {
        let dq = DialogQueue::new();
        let (open, title, body) = dq.snapshot();
        assert!(!open);
        assert!(title.is_empty());
        assert!(body.is_empty());

        let _rx = dq.confirm("My Title", "My Body");
        let (open, title, body) = dq.snapshot();
        assert!(open);
        assert_eq!(title, "My Title");
        assert_eq!(body, "My Body");
    }

    #[test]
    fn resolve_without_open_is_noop() {
        let dq = DialogQueue::new();
        // Should not panic.
        dq.resolve_confirmed();
        dq.resolve_cancelled();
        assert!(!dq.is_open());
    }
}
