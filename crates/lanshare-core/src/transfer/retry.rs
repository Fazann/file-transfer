//! Reconnect backoff for the sender: wait 1 s, 2 s, 5 s, 10 s, then every 10 s, for up to 2 minutes.

use std::time::Duration;

use tokio::time::Instant;

/// Waits between reconnect attempts. After the last entry, the last delay repeats.
const DELAYS: [Duration; 4] =
    [Duration::from_secs(1), Duration::from_secs(2), Duration::from_secs(5), Duration::from_secs(10)];

/// How long to keep trying after the connection dropped.
pub const RETRY_WINDOW: Duration = Duration::from_secs(120);

/// Hands out the delay before each reconnect attempt, until [`RETRY_WINDOW`] has passed.
#[derive(Debug, Clone)]
pub(crate) struct Backoff {
    attempt: usize,
    deadline: Instant,
}

impl Backoff {
    /// Starts a new retry window at `now`.
    pub(crate) fn new(now: Instant) -> Self {
        Self { attempt: 0, deadline: now + RETRY_WINDOW }
    }

    /// The delay before the next attempt, or `None` if waiting it would end past the deadline.
    pub(crate) fn next_delay(&mut self, now: Instant) -> Option<Duration> {
        let delay = DELAYS[self.attempt.min(DELAYS.len() - 1)];
        if now + delay > self.deadline {
            return None;
        }
        self.attempt += 1;
        Some(delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_grow_then_repeat_until_the_window_ends() {
        let start = Instant::now();
        let mut backoff = Backoff::new(start);
        let mut now = start;
        let mut delays = Vec::new();
        while let Some(delay) = backoff.next_delay(now) {
            delays.push(delay.as_secs());
            now += delay;
        }
        // 1 + 2 + 5 + 10 * 11 = 118 seconds; one more 10 s wait would pass 120.
        assert_eq!(delays, [1, 2, 5, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10]);
    }

    #[test]
    fn time_spent_connecting_counts_against_the_window() {
        let start = Instant::now();
        let mut backoff = Backoff::new(start);
        assert_eq!(backoff.next_delay(start + Duration::from_secs(119)), Some(Duration::from_secs(1)));
        assert_eq!(backoff.next_delay(start + Duration::from_secs(119)), None);
    }
}
