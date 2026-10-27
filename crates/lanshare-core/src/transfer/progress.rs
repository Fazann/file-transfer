//! Transfer progress, published through a `tokio::sync::watch` channel.
//!
//! A `watch` channel keeps only the latest value, so a slow UI never makes the transfer wait. Callers compute speed
//! and ETA themselves; core does not format anything.

use tokio::sync::watch;

/// Bytes moved so far out of the total.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    /// Bytes the receiver has verified so far, including `resumed`.
    pub transferred: u64,
    /// Total size of the file in bytes.
    pub total: u64,
    /// Bytes the receiver already had when the current connection started. Zero for a fresh transfer.
    pub resumed: u64,
}

/// Creates a progress channel starting at zero. Pass the sender to a transfer and watch the receiver.
#[must_use]
pub fn progress_channel() -> (watch::Sender<Progress>, watch::Receiver<Progress>) {
    watch::channel(Progress::default())
}
