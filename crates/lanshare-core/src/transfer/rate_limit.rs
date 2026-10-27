//! Bandwidth limit in MB/s (1 MB = 1,000,000 bytes), shared by every data stream of a transfer and changeable while
//! it runs.

use std::{
    num::NonZeroU32,
    sync::{Arc, RwLock},
};

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};

use super::BUFFER_SIZE;

/// The limiter counts in cells of 1 KB, so a `u32` cell count covers any realistic limit.
const CELL_BYTES: usize = 1000;

/// A shared, adjustable bandwidth limit. Clones share the same limit.
///
/// `governor` implements the "generic cell rate algorithm": each write waits until enough of the per-second budget
/// has built up. The burst allowance is a tenth of a second of traffic, so the long-run rate stays close to the
/// limit.
#[derive(Debug, Clone, Default)]
pub struct RateLimit {
    inner: Arc<RwLock<Limit>>,
}

#[derive(Debug, Default)]
struct Limit {
    mb_per_second: u32,
    limiter: Option<Arc<DefaultDirectRateLimiter>>,
}

impl RateLimit {
    /// A limit of `mb_per_second` MB/s. `0` means unlimited.
    #[must_use]
    pub fn new(mb_per_second: u32) -> Self {
        let limit = Self::default();
        limit.set(mb_per_second);
        limit
    }

    /// Changes the limit. Writes already waiting finish under the old limit. `0` means unlimited.
    pub fn set(&self, mb_per_second: u32) {
        let limiter = limiter_for(mb_per_second).map(Arc::new);
        if let Ok(mut limit) = self.inner.write() {
            *limit = Limit { mb_per_second, limiter };
        }
    }

    /// The current limit in MB/s, `0` for unlimited.
    #[must_use]
    pub fn get(&self) -> u32 {
        self.inner.read().map_or(0, |limit| limit.mb_per_second)
    }

    /// Waits until `bytes` more may be sent. `bytes` must not exceed the 256 KiB write slice.
    pub(crate) async fn wait_for(&self, bytes: usize) {
        // Clone the limiter out so the lock is not held while waiting.
        let limiter = self.inner.read().ok().and_then(|limit| limit.limiter.clone());
        let Some(limiter) = limiter else { return };
        let cells = u32::try_from(bytes.div_ceil(CELL_BYTES)).unwrap_or(u32::MAX).max(1);
        if let Some(cells) = NonZeroU32::new(cells) {
            // Fails only if `cells` is above the burst size, which `limiter_for` rules out for one write slice.
            let _ = limiter.until_n_ready(cells).await;
        }
    }
}

fn limiter_for(mb_per_second: u32) -> Option<DefaultDirectRateLimiter> {
    let cells_per_second = NonZeroU32::new(mb_per_second.saturating_mul(1000))?;
    let slice_cells = u32::try_from(BUFFER_SIZE.div_ceil(CELL_BYTES)).unwrap_or(u32::MAX);
    let burst = (cells_per_second.get() / 10).max(slice_cells);
    let quota = Quota::per_second(cells_per_second).allow_burst(NonZeroU32::new(burst)?);
    Some(RateLimiter::direct(quota))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[tokio::test]
    async fn unlimited_never_waits() {
        let limit = RateLimit::new(0);
        let start = Instant::now();
        for _ in 0..1000 {
            limit.wait_for(BUFFER_SIZE).await;
        }
        assert!(start.elapsed().as_millis() < 100);
    }

    #[tokio::test]
    async fn limit_holds_and_can_change() {
        // About 10 MB at 20 MB/s: the first 2 MB pass at once (the burst), the other 8 MB take 0.4 s.
        let limit = RateLimit::new(20);
        assert_eq!(limit.get(), 20);
        let start = Instant::now();
        for _ in 0..(10_000_000 / BUFFER_SIZE) {
            limit.wait_for(BUFFER_SIZE).await;
        }
        let elapsed = start.elapsed().as_secs_f64();
        assert!((0.35..0.5).contains(&elapsed), "took {elapsed} s");

        limit.set(0);
        let start = Instant::now();
        limit.wait_for(BUFFER_SIZE).await;
        assert!(start.elapsed().as_millis() < 50, "removing the limit applies at once");
    }
}
