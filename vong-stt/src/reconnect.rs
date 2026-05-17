//! Exponential backoff for WebSocket reconnect.
//!
//! Plan v2 Section 13.7.1 — exponential backoff 1s → 2s → 4s → 8s → 16s → 30s,
//! max 3 attempts then degrade gracefully.

use std::time::Duration;

/// Exponential backoff iterator with cap and attempt limit.
#[derive(Debug)]
pub struct ExponentialBackoff {
    attempts: u32,
    max_attempts: u32,
    cap: Duration,
}

impl ExponentialBackoff {
    /// Construct with max attempts and cap.
    ///
    /// Default plan v2: 3 attempts, cap 30s.
    pub fn new(max_attempts: u32, cap: Duration) -> Self {
        Self {
            attempts: 0,
            max_attempts,
            cap,
        }
    }

    /// Compute next delay and advance attempt counter.
    ///
    /// Returns `None` if attempts exhausted.
    pub fn next_delay(&mut self) -> Option<Duration> {
        if self.attempts >= self.max_attempts {
            return None;
        }
        // 2^attempts seconds, capped
        let secs = 2u64.saturating_pow(self.attempts);
        let delay = Duration::from_secs(secs).min(self.cap);
        self.attempts = self.attempts.saturating_add(1);
        Some(delay)
    }

    /// Reset attempt counter (call after successful reconnect).
    pub fn reset(&mut self) {
        self.attempts = 0;
    }

    /// Number of attempts made so far.
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// True if next call to `next_delay` will return None.
    pub fn exhausted(&self) -> bool {
        self.attempts >= self.max_attempts
    }
}

impl Default for ExponentialBackoff {
    fn default() -> Self {
        // Plan v2 default: 3 attempts, 30s cap
        Self::new(3, Duration::from_secs(30))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles() {
        let mut b = ExponentialBackoff::new(5, Duration::from_secs(60));
        assert_eq!(b.next_delay(), Some(Duration::from_secs(1))); // 2^0
        assert_eq!(b.next_delay(), Some(Duration::from_secs(2))); // 2^1
        assert_eq!(b.next_delay(), Some(Duration::from_secs(4))); // 2^2
        assert_eq!(b.next_delay(), Some(Duration::from_secs(8))); // 2^3
        assert_eq!(b.next_delay(), Some(Duration::from_secs(16))); // 2^4
    }

    #[test]
    fn backoff_respects_cap() {
        let mut b = ExponentialBackoff::new(10, Duration::from_secs(10));
        // Past 2^3=8, capped at 10
        let _ = b.next_delay(); // 1
        let _ = b.next_delay(); // 2
        let _ = b.next_delay(); // 4
        let _ = b.next_delay(); // 8
        assert_eq!(b.next_delay(), Some(Duration::from_secs(10))); // 16 capped → 10
    }

    #[test]
    fn backoff_exhausts() {
        let mut b = ExponentialBackoff::new(2, Duration::from_secs(30));
        assert!(b.next_delay().is_some());
        assert!(b.next_delay().is_some());
        assert_eq!(b.next_delay(), None);
        assert!(b.exhausted());
    }

    #[test]
    fn backoff_reset() {
        let mut b = ExponentialBackoff::new(2, Duration::from_secs(30));
        let _ = b.next_delay();
        let _ = b.next_delay();
        assert!(b.exhausted());
        b.reset();
        assert!(!b.exhausted());
        assert_eq!(b.next_delay(), Some(Duration::from_secs(1)));
    }

    #[test]
    fn default_is_three_attempts_30s_cap() {
        let mut b = ExponentialBackoff::default();
        let mut count = 0;
        while b.next_delay().is_some() {
            count += 1;
        }
        assert_eq!(count, 3);
    }
}
