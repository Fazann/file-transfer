//! Transfer progress, published through a `tokio::sync::watch` channel.
//!
//! A `watch` channel keeps only the latest value, so a slow UI never makes the transfer wait. Callers compute speed
//! and ETA themselves; core does not format anything.

use tokio::sync::watch;

/// Bytes moved so far out of the total.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    /// Bytes sent or received so far.
    pub transferred: u64,
    /// Total size of the file in bytes.
    pub total: u64,
}

/// Creates a progress channel starting at zero. Pass the sender to a transfer and watch the receiver.
#[must_use]
pub fn progress_channel() -> (watch::Sender<Progress>, watch::Receiver<Progress>) {
    watch::channel(Progress::default())
}
