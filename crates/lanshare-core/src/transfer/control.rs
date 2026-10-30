//! Controlling a running transfer: pause, resume, cancel and bandwidth limit, plus the shared pieces both sides use to keep
//! the peer informed and to notice a dead connection.

use std::{
    convert::Infallible,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex as StdMutex,
    },
    time::Duration,
};

use tokio::{
    sync::{watch, Mutex},
    time::Instant,
};
use tracing::info;

use super::{rate_limit::RateLimit, write_frame, IDLE_TIMEOUT};
use crate::{
    error::{Error, Result},
    protocol::Message,
    transport::SendHalf,
};

/// Handle to pause, resume, cancel and throttle a transfer from outside, for example from the UI.
///
/// Clones control the same transfer. One handle may be used across reconnects: a transfer paused before a
/// connection dropped stays paused on the next one.
#[derive(Debug, Clone)]
pub struct TransferControl {
    paused: watch::Sender<bool>,
    cancelled: watch::Sender<bool>,
    rate_limit: RateLimit,
}

impl Default for TransferControl {
    fn default() -> Self {
        Self::new(0)
    }
}

impl TransferControl {
    /// A running (not paused) transfer limited to `bandwidth_limit_mbps` MB/s; `0` means unlimited.
    #[must_use]
    pub fn new(bandwidth_limit_mbps: u32) -> Self {
        Self {
            paused: watch::Sender::new(false),
            cancelled: watch::Sender::new(false),
            rate_limit: RateLimit::new(bandwidth_limit_mbps),
        }
    }

    /// Cancels the transfer: it stops with [`Error::Cancelled`] and the other device is told, so it does not wait
    /// for a reconnect. A partly received file is kept, so offering it again later still resumes.
    pub fn cancel(&self) {
        self.cancelled.send_replace(true);
    }

    /// Whether [`TransferControl::cancel`] was called.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow()
    }

    /// Completes once the transfer is cancelled.
    pub(crate) async fn cancelled(&self) {
        let mut cancelled = self.cancelled.subscribe();
        // Fails only if the sender is dropped, which cannot happen while `self` holds it.
        let _ = cancelled.wait_for(|cancelled| *cancelled).await;
    }

    /// Pauses: the sender stops starting new chunks, and the other device is told.
    pub fn pause(&self) {
        self.paused.send_replace(true);
    }

    /// Continues after [`TransferControl::pause`], or after the other device paused.
    pub fn resume(&self) {
        self.paused.send_replace(false);
    }

    /// Whether the transfer is paused by either device.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        *self.paused.borrow()
    }

    /// Watches the paused state, which changes when either device pauses or resumes.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.paused.subscribe()
    }

    /// Changes the bandwidth limit while the transfer runs. Only the sending side applies it.
    pub fn set_bandwidth_limit(&self, mbps: u32) {
        self.rate_limit.set(mbps);
    }

    /// The current bandwidth limit in MB/s, `0` for unlimited.
    #[must_use]
    pub fn bandwidth_limit(&self) -> u32 {
        self.rate_limit.get()
    }

    pub(crate) fn rate_limit(&self) -> &RateLimit {
        &self.rate_limit
    }
}

/// The writing half of the control channel, shared by everything in a session that sends control messages.
///
/// The `tokio` mutex is held across the `await` of a write, so two messages never interleave.
pub(crate) struct ControlWriter(Mutex<SendHalf>);

impl ControlWriter {
    pub(crate) fn new(send: SendHalf) -> Self {
        Self(Mutex::new(send))
    }

    pub(crate) async fn send(&self, message: &Message) -> Result<()> {
        write_frame(&mut *self.0.lock().await, message).await
    }

    /// Closes the writing direction, so the peer reads a clean end of stream.
    pub(crate) async fn shutdown(&self) {
        // The connection may already be gone; there is nothing useful to do about a failure here.
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut *self.0.lock().await).await;
    }
}

/// Keeps both devices' paused state in step during one session.
///
/// `peer_knows` is what the other device was last told or told us. A local change is sent only when it differs,
/// so a `Pause` received from the peer is not echoed back.
pub(crate) struct PauseSync<'a> {
    control: &'a TransferControl,
    peer_knows: AtomicBool,
}

impl<'a> PauseSync<'a> {
    pub(crate) fn new(control: &'a TransferControl) -> Self {
        Self { control, peer_knows: AtomicBool::new(false) }
    }

    /// Applies a `Pause` or `Resume` received from the peer.
    pub(crate) fn apply_from_peer(&self, paused: bool) {
        info!(paused, "the other device changed the paused state");
        self.peer_knows.store(paused, Ordering::SeqCst);
        self.control.paused.send_replace(paused);
    }

    /// Sends `Pause` or `Resume` whenever the local state changes. Runs until the session ends or a write fails.
    pub(crate) async fn run(&self, writer: &ControlWriter) -> Result<Infallible> {
        let mut paused = self.control.subscribe();
        loop {
            let now = *paused.borrow_and_update();
            if self.peer_knows.swap(now, Ordering::SeqCst) != now {
                writer.send(if now { &Message::Pause } else { &Message::Resume }).await?;
            }
            if paused.changed().await.is_err() {
                // Cannot happen while `self.control` holds the sender; wait for the session to end instead.
                std::future::pending::<()>().await;
            }
        }
    }
}

/// When data last moved, to notice a peer that went silent.
#[derive(Debug)]
pub(crate) struct Activity(StdMutex<Instant>);

impl Activity {
    pub(crate) fn new() -> Self {
        Self(StdMutex::new(Instant::now()))
    }

    /// Records that data moved just now.
    pub(crate) fn touch(&self) {
        if let Ok(mut last) = self.0.lock() {
            *last = Instant::now();
        }
    }

    fn idle_for(&self) -> Duration {
        self.0.lock().map_or(Duration::ZERO, |last| last.elapsed())
    }

    /// Fails with [`Error::ConnectionLost`] once nothing moved for [`IDLE_TIMEOUT`] while not paused.
    ///
    /// While paused, nothing is expected to move; the transports' own keep-alives notice a dead peer then.
    pub(crate) async fn watchdog(&self, control: &TransferControl) -> Result<Infallible> {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if control.is_paused() {
                self.touch();
            } else if self.idle_for() > IDLE_TIMEOUT {
                return Err(Error::ConnectionLost);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{future::Future, pin::Pin};

    use tokio::io::{duplex, DuplexStream};

    use super::*;
    use crate::protocol::read_message;

    /// Reads the next control message while `run` keeps going.
    async fn next<F: Future<Output = Result<Infallible>>>(server: &mut DuplexStream, run: &mut Pin<&mut F>) -> Message {
        tokio::select! {
            message = read_message(server) => message.unwrap(),
            _ = run.as_mut() => unreachable!("the pause sync only stops on errors"),
        }
    }

    #[tokio::test]
    async fn local_changes_are_sent_but_peer_changes_are_not_echoed() {
        let control = TransferControl::default();
        let sync = PauseSync::new(&control);
        let (client, mut server) = duplex(1024);
        let writer = ControlWriter::new(Box::new(client));

        let run = sync.run(&writer);
        tokio::pin!(run);
        control.pause();
        assert_eq!(next(&mut server, &mut run).await, Message::Pause);
        control.resume();
        assert_eq!(next(&mut server, &mut run).await, Message::Resume);

        sync.apply_from_peer(true);
        assert!(control.is_paused());
        control.resume();
        assert_eq!(
            next(&mut server, &mut run).await,
            Message::Resume,
            "only the local resume is sent, not the peer's pause"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_fires_only_when_not_paused() {
        let control = TransferControl::default();
        let activity = Activity::new();
        control.pause();
        let result = tokio::time::timeout(IDLE_TIMEOUT * 3, activity.watchdog(&control)).await;
        assert!(result.is_err(), "no timeout while paused");
        control.resume();
        let error = activity.watchdog(&control).await.unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::ConnectionLost);
    }
}
