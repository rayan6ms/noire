//! Bounded repeated-event rate limiting.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

const MAX_EVENT_KEYS: usize = 64;

/// Fixed-cardinality limiter for stable structured event names.
#[derive(Debug)]
pub struct EventRateLimiter {
    interval: Duration,
    last: BTreeMap<&'static str, Instant>,
}

impl EventRateLimiter {
    /// Creates a limiter with the minimum repeat interval.
    #[must_use]
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: BTreeMap::new(),
        }
    }

    /// Returns true when a stable event should be emitted now.
    pub fn should_emit(&mut self, event: &'static str, now: Instant) -> bool {
        if let Some(previous) = self.last.get_mut(event) {
            if now.saturating_duration_since(*previous) < self.interval {
                return false;
            }
            *previous = now;
            return true;
        }
        if self.last.len() >= MAX_EVENT_KEYS {
            return false;
        }
        self.last.insert(event, now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_events_are_bounded_and_rate_limited() {
        let origin = Instant::now();
        let mut limiter = EventRateLimiter::new(Duration::from_secs(5));
        assert!(limiter.should_emit("audio.unavailable", origin));
        assert!(!limiter.should_emit("audio.unavailable", origin + Duration::from_secs(1)));
        assert!(limiter.should_emit("audio.unavailable", origin + Duration::from_secs(5)));
    }
}
