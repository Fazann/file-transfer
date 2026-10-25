//! mDNS: registers this device as `_lanshare._tcp.local.` and browses for others.
//!
//! `mdns-sd` caches answers and only reports a service again when it changes, so every [`REFRESH_INTERVAL`] this
//! module browses again (which replays the cache and refreshes `last_seen`) and asks the cache to verify each known
//! peer. A peer that stopped answering is flushed within about 10 seconds and reported as lost.

use std::{
    collections::HashMap,
    net::IpAddr,
    time::{Duration, Instant},
};

use mdns_sd::{Receiver, ServiceDaemon, ServiceEvent, ServiceInfo, VERIFY_TIMEOUT_DEFAULT};
use tokio::{task::JoinHandle, time};
use tracing::{debug, warn};

use super::{peers::Peer, PeerSink, SERVICE_TYPE};
use crate::{
    error::{Error, Result},
    identity::{DeviceId, DeviceInfo, Os},
};

/// How often to replay the cache and verify known peers.
const REFRESH_INTERVAL: Duration = Duration::from_secs(10);

/// How long shutdown waits for the goodbye packet to be sent.
const UNREGISTER_TIMEOUT: Duration = Duration::from_secs(1);

/// The mDNS daemon (it runs on its own thread) plus our browsing task.
pub(super) struct MdnsDiscovery {
    daemon: ServiceDaemon,
    registered: Option<String>,
    browser: JoinHandle<()>,
}

impl MdnsDiscovery {
    /// Starts the daemon, registers `info` if `advertise`, and starts browsing.
    pub(super) fn start(info: &DeviceInfo, advertise: bool, sink: PeerSink) -> Result<Self> {
        let daemon = ServiceDaemon::new().map_err(|error| mdns_error(&error))?;
        let registered = if advertise { Some(register(&daemon, info)?) } else { None };
        let browser = tokio::spawn(browse_loop(daemon.clone(), sink));
        Ok(Self { daemon, registered, browser })
    }

    /// Unregisters (sends an mDNS goodbye) and stops the daemon.
    pub(super) async fn shutdown(mut self) {
        self.browser.abort();
        if let Some(fullname) = self.registered.take() {
            match self.daemon.unregister(&fullname) {
                Ok(status) => {
                    if time::timeout(UNREGISTER_TIMEOUT, status.recv_async()).await.is_err() {
                        debug!("timed out waiting for mDNS unregister");
                    }
                }
                Err(error) => warn!(%error, "mDNS unregister failed"),
            }
        }
        // `Drop` below stops the daemon.
    }
}

impl Drop for MdnsDiscovery {
    fn drop(&mut self) {
        self.browser.abort();
        if let Err(error) = self.daemon.shutdown() {
            debug!(%error, "mDNS daemon already stopped");
        }
    }
}

fn register(daemon: &ServiceDaemon, info: &DeviceInfo) -> Result<String> {
    // The instance name must be unique on the network, so use the device id; the display name goes in TXT.
    let instance = info.id.to_string();
    let host = format!("{instance}.local.");
    let properties = [
        ("id", instance.clone()),
        ("name", info.name.clone()),
        ("v", info.protocol_version.to_string()),
        ("os", info.os.as_str().to_owned()),
    ];
    // `()` means "no fixed addresses"; `enable_addr_auto` keeps them in sync with the network interfaces.
    let service = ServiceInfo::new(SERVICE_TYPE, &instance, &host, (), info.port, &properties[..])
        .map_err(|error| mdns_error(&error))?
        .enable_addr_auto();
    let fullname = service.get_fullname().to_owned();
    daemon.register(service).map_err(|error| mdns_error(&error))?;
    Ok(fullname)
}

async fn browse_loop(daemon: ServiceDaemon, sink: PeerSink) {
    // mDNS full name -> device id, so `ServiceRemoved` (which only has the name) can be mapped to a peer.
    let mut known: HashMap<String, DeviceId> = HashMap::new();
    let mut refresh = time::interval(REFRESH_INTERVAL);
    let mut events: Option<Receiver<ServiceEvent>> = None;
    loop {
        // `select!` waits for whichever future finishes first, like Go's `select` over channels.
        tokio::select! {
            _ = refresh.tick() => {
                for fullname in known.keys() {
                    if let Err(error) = daemon.verify(fullname.clone(), VERIFY_TIMEOUT_DEFAULT) {
                        debug!(%error, "mDNS verify failed");
                    }
                }
                // Browsing again replaces the previous browse and replays every cached service.
                match daemon.browse(SERVICE_TYPE) {
                    Ok(receiver) => events = Some(receiver),
                    Err(error) => warn!(%error, "mDNS browse failed"),
                }
            }
            event = next_event(events.as_ref()) => match event {
                Some(event) => handle_event(event, &mut known, &sink),
                None => events = None,
            },
        }
    }
}

/// The next browse event, or never if there is no active browse.
async fn next_event(events: Option<&Receiver<ServiceEvent>>) -> Option<ServiceEvent> {
    match events {
        Some(receiver) => receiver.recv_async().await.ok(),
        None => std::future::pending().await,
    }
}

fn handle_event(event: ServiceEvent, known: &mut HashMap<String, DeviceId>, sink: &PeerSink) {
    match event {
        ServiceEvent::ServiceResolved(service) => {
            let addresses = service.get_addresses().iter().map(mdns_sd::ScopedIp::to_ip_addr).collect();
            let peer = peer_from_txt(|key| service.get_property_val_str(key), addresses, service.get_port());
            if let Some(peer) = peer {
                known.insert(service.get_fullname().to_owned(), peer.id);
                sink.seen(peer);
            } else {
                debug!(fullname = service.get_fullname(), "ignoring service with invalid TXT records");
            }
        }
        ServiceEvent::ServiceRemoved(_, fullname) => {
            if let Some(id) = known.remove(&fullname) {
                sink.gone(id);
            }
        }
        _ => {}
    }
}

/// Builds a peer from TXT records (`id`, `name`, `v`, `os`). Returns `None` if any is missing or invalid.
fn peer_from_txt<'a>(property: impl Fn(&str) -> Option<&'a str>, addresses: Vec<IpAddr>, port: u16) -> Option<Peer> {
    Some(Peer {
        id: DeviceId::parse(property("id")?)?,
        name: property("name")?.to_owned(),
        os: Os::parse(property("os")?)?,
        addresses,
        port,
        protocol_version: property("v")?.parse().ok()?,
        last_seen: Instant::now(),
    })
}

fn mdns_error(error: &mdns_sd::Error) -> Error {
    Error::Io(std::io::Error::other(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    #[test]
    fn txt_records_become_a_peer() {
        let id = DeviceId::new_random();
        let id_text = id.to_string();
        let txt: HashMap<&str, &str> = [("id", id_text.as_str()), ("name", "Desk"), ("v", "2"), ("os", "macos")].into();
        let address = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9));
        let peer = peer_from_txt(|key| txt.get(key).copied(), vec![address], 52000).unwrap();
        assert_eq!((peer.id, peer.name.as_str(), peer.os, peer.protocol_version), (id, "Desk", Os::MacOs, 2));
        assert_eq!(peer.addresses, vec![address]);
    }

    #[test]
    fn invalid_txt_records_are_ignored() {
        let txt: HashMap<&str, &str> = [("id", "nope"), ("name", "Desk"), ("v", "2"), ("os", "macos")].into();
        assert!(peer_from_txt(|key| txt.get(key).copied(), vec![], 52000).is_none());
        let txt: HashMap<&str, &str> = [("name", "Desk")].into();
        assert!(peer_from_txt(|key| txt.get(key).copied(), vec![], 52000).is_none());
    }
}
