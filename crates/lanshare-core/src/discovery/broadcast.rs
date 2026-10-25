//! UDP broadcast fallback for networks that block mDNS: announce this device every 5 seconds on UDP port 52001 and
//! listen for announcements from others.
//!
//! Announcements go to each interface's own broadcast address (e.g. `192.168.1.255`), because macOS refuses to send
//! to `255.255.255.255` ("no route to host"). `255.255.255.255` is still tried as a last resort.

use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::{net::UdpSocket, task::JoinHandle, time};
use tracing::{debug, warn};

use super::{peers::Peer, PeerSink};
use crate::{
    error::{Error, Result},
    identity::{DeviceId, DeviceInfo, Os},
};

/// First bytes of every announcement, so unrelated traffic on the port is ignored cheaply.
const MAGIC: [u8; 4] = *b"LNSH";

/// How often this device announces itself.
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(5);

/// Receive buffer size. Announcements are far smaller (device names are capped at 200 bytes).
const MAX_DATAGRAM_SIZE: usize = 1024;

/// How long to pause after a socket error before receiving again, so a broken socket does not spin the CPU.
const ERROR_BACKOFF: Duration = Duration::from_secs(1);

/// The UDP announcement: [`MAGIC`] followed by this struct encoded with `postcard`. Same fields as the mDNS TXT
/// records, plus the port (mDNS carries it in the SRV record) and a `leaving` flag sent once on shutdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    /// Wire protocol version. Always the first field.
    pub protocol_version: u16,
    /// Stable device id.
    pub id: DeviceId,
    /// Human-readable device name.
    pub name: String,
    /// Operating system.
    pub os: Os,
    /// TCP port the device receives transfers on.
    pub port: u16,
    /// `true` in the last announcement before the device stops.
    pub leaving: bool,
}

impl Announcement {
    /// The announcement for `info`.
    #[must_use]
    pub fn new(info: &DeviceInfo, leaving: bool) -> Self {
        Self {
            protocol_version: info.protocol_version,
            id: info.id,
            name: info.name.clone(),
            os: info.os,
            port: info.port,
            leaving,
        }
    }

    /// Encodes the announcement as one datagram.
    ///
    /// # Errors
    ///
    /// [`Error::ProtocolViolation`] if encoding fails, which only happens on a bug.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut datagram = MAGIC.to_vec();
        let payload = postcard::to_allocvec(self)
            .map_err(|error| Error::protocol_violation(format!("cannot encode announcement: {error}")))?;
        datagram.extend_from_slice(&payload);
        Ok(datagram)
    }

    /// Decodes a datagram. Returns `None` for anything that is not a valid announcement.
    #[must_use]
    pub fn decode(datagram: &[u8]) -> Option<Self> {
        let payload = datagram.strip_prefix(&MAGIC)?;
        postcard::from_bytes(payload).ok()
    }

    fn into_peer(self, address: IpAddr) -> Peer {
        Peer {
            id: self.id,
            name: self.name,
            os: self.os,
            addresses: vec![address],
            port: self.port,
            protocol_version: self.protocol_version,
            last_seen: Instant::now(),
        }
    }
}

/// The broadcast socket and its announce/listen tasks.
pub(super) struct BroadcastDiscovery {
    socket: Arc<UdpSocket>,
    port: u16,
    goodbye: Option<Vec<u8>>,
    tasks: Vec<JoinHandle<()>>,
}

impl BroadcastDiscovery {
    /// Binds UDP `port`, starts listening, and starts announcing `info` if `advertise`.
    pub(super) fn start(info: &DeviceInfo, advertise: bool, port: u16, sink: PeerSink) -> Result<Self> {
        let socket = Arc::new(bind_socket(port)?);
        let mut tasks = vec![tokio::spawn(listen_loop(Arc::clone(&socket), sink))];
        let mut goodbye = None;
        if advertise {
            let hello = Announcement::new(info, false).encode()?;
            tasks.push(tokio::spawn(announce_loop(Arc::clone(&socket), hello, port)));
            goodbye = Some(Announcement::new(info, true).encode()?);
        }
        Ok(Self { socket, port, goodbye, tasks })
    }

    /// Stops the tasks and, if this device was announcing, sends one `leaving` announcement.
    pub(super) async fn shutdown(mut self) {
        self.abort_tasks();
        if let Some(goodbye) = self.goodbye.take() {
            send_to_all(&self.socket, &goodbye, self.port).await;
        }
    }

    fn abort_tasks(&self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Drop for BroadcastDiscovery {
    fn drop(&mut self) {
        self.abort_tasks();
    }
}

/// Binds a broadcast-capable UDP socket that other `LanShare` processes on this machine can bind too.
///
/// tokio cannot set `SO_REUSEADDR`/`SO_REUSEPORT` before binding, so the socket is built with `socket2` and then
/// handed to tokio.
fn bind_socket(port: u16) -> Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).map_err(Error::Io)?;
    socket.set_reuse_address(true).map_err(Error::Io)?;
    #[cfg(unix)]
    socket.set_reuse_port(true).map_err(Error::Io)?;
    socket.set_broadcast(true).map_err(Error::Io)?;
    socket.set_nonblocking(true).map_err(Error::Io)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into()).map_err(Error::Io)?;
    UdpSocket::from_std(socket.into()).map_err(Error::Io)
}

async fn announce_loop(socket: Arc<UdpSocket>, datagram: Vec<u8>, port: u16) {
    // The first tick fires immediately, so peers see us right away.
    let mut ticks = time::interval(ANNOUNCE_INTERVAL);
    loop {
        ticks.tick().await;
        send_to_all(&socket, &datagram, port).await;
    }
}

/// Sends `datagram` to every broadcast address. Interfaces are listed again each time, since Wi-Fi comes and goes.
async fn send_to_all(socket: &UdpSocket, datagram: &[u8], port: u16) {
    let interfaces = match if_addrs::get_if_addrs() {
        Ok(interfaces) => interfaces,
        Err(error) => {
            debug!(%error, "cannot list network interfaces");
            Vec::new()
        }
    };
    let ipv4_interfaces: Vec<(Ipv4Addr, Ipv4Addr)> = interfaces
        .into_iter()
        .filter_map(|interface| match interface.addr {
            if_addrs::IfAddr::V4(v4) => Some((v4.ip, v4.netmask)),
            if_addrs::IfAddr::V6(_) => None,
        })
        .collect();
    for target in broadcast_targets(&ipv4_interfaces) {
        if let Err(error) = socket.send_to(datagram, SocketAddr::from((target, port))).await {
            // Normal while offline, and for 255.255.255.255 on macOS.
            debug!(%target, %error, "broadcast announcement failed");
        }
    }
}

/// The directed broadcast address of each `(ip, netmask)` interface, then `255.255.255.255`.
///
/// Loopback and single-host (`/32`) interfaces are skipped. The broadcast address is the IP with every host bit set:
/// `ip | !netmask`, e.g. `192.168.1.20` with `255.255.255.0` gives `192.168.1.255`.
fn broadcast_targets(interfaces: &[(Ipv4Addr, Ipv4Addr)]) -> Vec<Ipv4Addr> {
    let mut targets = Vec::new();
    for (ip, netmask) in interfaces {
        if ip.is_loopback() || *netmask == Ipv4Addr::BROADCAST {
            continue;
        }
        let target = Ipv4Addr::from(ip.to_bits() | !netmask.to_bits());
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets.push(Ipv4Addr::BROADCAST);
    targets
}

async fn listen_loop(socket: Arc<UdpSocket>, sink: PeerSink) {
    let mut buffer = vec![0; MAX_DATAGRAM_SIZE];
    loop {
        match socket.recv_from(&mut buffer).await {
            Ok((length, from)) => handle_datagram(&buffer[..length], from.ip(), &sink),
            Err(error) => {
                // Windows reports ICMP "port unreachable" from earlier sends as a receive error; just carry on.
                if error.kind() != io::ErrorKind::ConnectionReset {
                    warn!(%error, "broadcast receive failed");
                }
                time::sleep(ERROR_BACKOFF).await;
            }
        }
    }
}

fn handle_datagram(datagram: &[u8], from: IpAddr, sink: &PeerSink) {
    match Announcement::decode(datagram) {
        Some(announcement) if announcement.leaving => sink.gone(announcement.id),
        Some(announcement) => sink.seen(announcement.into_peer(from)),
        None => debug!(%from, "ignoring unknown datagram"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{PeerEvent, PeerTable};

    fn info(name: &str) -> DeviceInfo {
        DeviceInfo {
            id: DeviceId::new_random(),
            name: name.to_owned(),
            os: Os::Linux,
            port: 52000,
            protocol_version: 2,
        }
    }

    #[test]
    fn announcement_round_trips() {
        let announcement = Announcement::new(&info("Desk"), false);
        let datagram = announcement.encode().unwrap();
        assert!(datagram.starts_with(&MAGIC));
        assert_eq!(Announcement::decode(&datagram), Some(announcement));
    }

    #[test]
    fn foreign_datagrams_are_ignored() {
        assert_eq!(Announcement::decode(b""), None);
        assert_eq!(Announcement::decode(b"hello world"), None);
        assert_eq!(Announcement::decode(b"LNSH\xff\xff"), None);
    }

    #[test]
    fn broadcast_targets_use_each_interface_subnet() {
        let interfaces = [
            (Ipv4Addr::LOCALHOST, Ipv4Addr::new(255, 0, 0, 0)),
            (Ipv4Addr::new(192, 168, 1, 20), Ipv4Addr::new(255, 255, 255, 0)),
            (Ipv4Addr::new(192, 168, 1, 21), Ipv4Addr::new(255, 255, 255, 0)),
            (Ipv4Addr::new(172, 20, 10, 2), Ipv4Addr::new(255, 255, 255, 240)),
            (Ipv4Addr::new(10, 8, 0, 5), Ipv4Addr::BROADCAST),
        ];
        assert_eq!(
            broadcast_targets(&interfaces),
            [Ipv4Addr::new(192, 168, 1, 255), Ipv4Addr::new(172, 20, 10, 15), Ipv4Addr::BROADCAST]
        );
        assert_eq!(broadcast_targets(&[]), [Ipv4Addr::BROADCAST]);
    }

    #[tokio::test]
    async fn listener_feeds_announcements_into_the_table() {
        let own = info("me");
        let (sink, mut events) = PeerSink::new(PeerTable::new(own.id));
        let port = {
            // Ask the OS for a free port, then release it for the discovery socket.
            let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            probe.local_addr().unwrap().port()
        };
        let discovery = BroadcastDiscovery::start(&own, false, port, sink).unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));

        let other = info("Desk");
        sender.send_to(&Announcement::new(&other, false).encode().unwrap(), target).await.unwrap();
        let found = time::timeout(Duration::from_secs(5), events.recv()).await.unwrap().unwrap();
        assert!(matches!(found, PeerEvent::Found(peer)
            if peer.id == other.id && peer.addresses == [IpAddr::V4(Ipv4Addr::LOCALHOST)]));

        sender.send_to(&Announcement::new(&other, true).encode().unwrap(), target).await.unwrap();
        let lost = time::timeout(Duration::from_secs(5), events.recv()).await.unwrap().unwrap();
        assert_eq!(lost, PeerEvent::Lost(other.id));
        drop(discovery);
    }
}
