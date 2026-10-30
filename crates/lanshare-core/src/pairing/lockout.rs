//! Limits PIN guessing: at most 3 failed pairings per peer address per minute, then pairing with that address is
//! refused for 5 minutes.
//!
//! Peers are counted by IP address, not by certificate, because an attacker can make new certificates for free.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

/// Failed attempts allowed within [`WINDOW`].
pub const MAX_FAILURES: usize = 3;

/// The period failures are counted over.
pub const WINDOW: Duration = Duration::from_secs(60);

/// How long pairing stays locked after too many failures.
pub const LOCK_DURATION: Duration = Duration::from_mins(5);

/// Failed pairing attempts per peer address. Shared by every connection a receiver handles.
#[derive(Debug, Default)]
pub struct PairingLockout {
    peers: Mutex<HashMap<IpAddr, Attempts>>,
}

#[derive(Debug, Default)]
struct Attempts {
    failures: Vec<Instant>,
    locked_until: Option<Instant>,
}

impl PairingLockout {
    /// An empty lockout table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `Err(seconds)` with the time left if `peer` is locked out at `now`, else `Ok`.
    ///
    /// # Errors
    ///
    /// The number of seconds until pairing is allowed again, rounded up.
    pub fn check(&self, peer: IpAddr, now: Instant) -> Result<(), u64> {
        let peers = self.peers.lock().unwrap_or_else(PoisonError::into_inner);
        match peers.get(&peer).and_then(|attempts| attempts.locked_until) {
            Some(until) if until > now => Err((until - now).as_secs() + 1),
            _ => Ok(()),
        }
    }

    /// Records a failed attempt by `peer` at `now`, locking it if this was one too many.
    pub fn record_failure(&self, peer: IpAddr, now: Instant) {
        let mut peers = self.peers.lock().unwrap_or_else(PoisonError::into_inner);
        let attempts = peers.entry(peer).or_default();
        attempts.failures.retain(|at| now.saturating_duration_since(*at) < WINDOW);
        attempts.failures.push(now);
        if attempts.failures.len() >= MAX_FAILURES {
            attempts.locked_until = Some(now + LOCK_DURATION);
            attempts.failures.clear();
        }
    }

    /// Forgets the failures of `peer` after it paired successfully.
    pub fn record_success(&self, peer: IpAddr) {
        self.peers.lock().unwrap_or_else(PoisonError::into_inner).remove(&peer);
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    const PEER: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));
    const OTHER: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 21));

    #[test]
    fn third_failure_within_a_minute_locks_for_five_minutes() {
        let lockout = PairingLockout::new();
        let start = Instant::now();
        lockout.record_failure(PEER, start);
        lockout.record_failure(PEER, start + Duration::from_secs(10));
        assert_eq!(lockout.check(PEER, start + Duration::from_secs(11)), Ok(()));

        lockout.record_failure(PEER, start + Duration::from_secs(20));
        assert_eq!(lockout.check(PEER, start + Duration::from_secs(20)), Err(301));
        assert_eq!(lockout.check(OTHER, start + Duration::from_secs(20)), Ok(()), "other peers are not affected");
        assert!(lockout.check(PEER, start + Duration::from_secs(300)).is_err());
        assert_eq!(lockout.check(PEER, start + Duration::from_secs(321)), Ok(()));
    }

    #[test]
    fn failures_older_than_a_minute_do_not_count() {
        let lockout = PairingLockout::new();
        let start = Instant::now();
        lockout.record_failure(PEER, start);
        lockout.record_failure(PEER, start + Duration::from_secs(30));
        lockout.record_failure(PEER, start + Duration::from_secs(61));
        assert_eq!(lockout.check(PEER, start + Duration::from_secs(61)), Ok(()));
    }

    #[test]
    fn success_clears_failures() {
        let lockout = PairingLockout::new();
        let start = Instant::now();
        lockout.record_failure(PEER, start);
        lockout.record_failure(PEER, start);
        lockout.record_success(PEER);
        lockout.record_failure(PEER, start);
        assert_eq!(lockout.check(PEER, start), Ok(()));
    }
}
