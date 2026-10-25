//! Finding other `LanShare` devices on the local network with mDNS, plus a UDP broadcast fallback for networks that
//! block multicast.
//!
//! [`Discovery::start`] runs everything in background tasks and reports changes as [`PeerEvent`]s.

mod broadcast;
mod mdns;
mod peers;

use std::{
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

pub use broadcast::Announcement;
pub use peers::{Peer, PeerEvent, PeerTable};
use tokio::{sync::mpsc, task::JoinHandle, time};
use tracing::{debug, trace, warn};

use crate::{error::Result, identity::DeviceInfo};

/// mDNS service type every `LanShare` device registers.
pub const SERVICE_TYPE: &str = "_lanshare._tcp.local.";

/// UDP port for broadcast announcements.
pub const BROADCAST_PORT: u16 = 52001;

/// A peer not heard from for this long is reported as lost.
pub const PEER_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the peer table is checked for expired peers.
const EXPIRY_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// What [`Discovery`] runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryOptions {
    /// Announce this device. `false` only listens, e.g. for `lanshare devices` or looking up a name.
    pub advertise: bool,
    /// Use mDNS.
    pub mdns: bool,
    /// Use UDP broadcast (`Config::udp_fallback`).
    pub udp_fallback: bool,
    /// UDP port for broadcast announcements. Normally [`BROADCAST_PORT`].
    pub broadcast_port: u16,
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        Self { advertise: true, mdns: true, udp_fallback: true, broadcast_port: BROADCAST_PORT }
    }
}

/// Running discovery: announces this device and tracks peers until [`Discovery::shutdown`] or drop.
pub struct Discovery {
    sink: PeerSink,
    mdns: Option<mdns::MdnsDiscovery>,
    broadcast: Option<broadcast::BroadcastDiscovery>,
    expiry: JoinHandle<()>,
}

impl Discovery {
    /// Starts mDNS and/or UDP broadcast discovery for the device `info`.
    ///
    /// Returns the running service and a channel of [`PeerEvent`]s. If mDNS fails to start but UDP broadcast is
    /// enabled, discovery continues with broadcast only.
    ///
    /// Must be called from inside a tokio runtime, because it spawns background tasks.
    ///
    /// # Errors
    ///
    /// [`Error::Io`](crate::Error::Io) if no discovery method could be started, for example when the UDP port
    /// cannot be bound.
    pub fn start(info: &DeviceInfo, options: DiscoveryOptions) -> Result<(Self, mpsc::UnboundedReceiver<PeerEvent>)> {
        let (sink, events) = PeerSink::new(PeerTable::new(info.id));
        let mdns = if options.mdns {
            match mdns::MdnsDiscovery::start(info, options.advertise, sink.clone()) {
                Ok(mdns) => Some(mdns),
                Err(error) if options.udp_fallback => {
                    warn!(%error, "mDNS unavailable, using UDP broadcast only");
                    None
                }
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let broadcast = if options.udp_fallback {
            Some(broadcast::BroadcastDiscovery::start(info, options.advertise, options.broadcast_port, sink.clone())?)
        } else {
            None
        };
        let expiry = tokio::spawn(expire_loop(sink.clone()));
        Ok((Self { sink, mdns, broadcast, expiry }, events))
    }

    /// Every peer currently known, sorted by name.
    #[must_use]
    pub fn peers(&self) -> Vec<Peer> {
        self.sink.lock().peers()
    }

    /// Stops discovery and tells other devices this one is leaving (mDNS goodbye and a UDP `leaving` announcement),
    /// so they show it as lost right away instead of after [`PEER_TIMEOUT`].
    pub async fn shutdown(mut self) {
        if let Some(mdns) = self.mdns.take() {
            mdns.shutdown().await;
        }
        if let Some(broadcast) = self.broadcast.take() {
            broadcast.shutdown().await;
        }
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        // Dropping a `JoinHandle` does not stop the task (unlike cancelling a Go context), so abort it explicitly.
        self.expiry.abort();
    }
}

async fn expire_loop(sink: PeerSink) {
    let mut ticks = time::interval(EXPIRY_CHECK_INTERVAL);
    loop {
        ticks.tick().await;
        if let Some(cutoff) = Instant::now().checked_sub(PEER_TIMEOUT) {
            sink.expire(cutoff);
        }
    }
}

/// Shared handle the mDNS and broadcast tasks use to update the peer table and publish events.
///
/// The table sits behind a `std::sync::Mutex` (not tokio's) because it is only locked briefly and never across an
/// `.await`.
#[derive(Clone)]
struct PeerSink {
    table: Arc<Mutex<PeerTable>>,
    events: mpsc::UnboundedSender<PeerEvent>,
}

impl PeerSink {
    fn new(table: PeerTable) -> (Self, mpsc::UnboundedReceiver<PeerEvent>) {
        let (events, receiver) = mpsc::unbounded_channel();
        (Self { table: Arc::new(Mutex::new(table)), events }, receiver)
    }

    fn seen(&self, peer: Peer) {
        let event = self.lock().upsert(peer);
        self.emit(event);
    }

    fn gone(&self, id: crate::identity::DeviceId) {
        let event = self.lock().remove(id);
        self.emit(event);
    }

    fn expire(&self, older_than: Instant) {
        let events = self.lock().expire(older_than);
        for event in events {
            self.emit(Some(event));
        }
    }

    fn lock(&self) -> MutexGuard<'_, PeerTable> {
        // A poisoned mutex means another thread panicked while holding it. The table is still consistent (every
        // update is a single map operation), so keep using it rather than panicking too.
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn emit(&self, event: Option<PeerEvent>) {
        if let Some(event) = event {
            debug!(?event, "peer event");
            if self.events.send(event).is_err() {
                trace!("nobody is listening for peer events");
            }
        }
    }
}
