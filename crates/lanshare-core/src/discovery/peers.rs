//! The peer table: every device seen recently, merged from mDNS and UDP broadcast. Pure logic, no networking.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    time::Instant,
};

use crate::identity::{DeviceId, Fingerprint, Os};

/// A device found on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    /// Stable device id.
    pub id: DeviceId,
    /// Human-readable device name.
    pub name: String,
    /// Operating system.
    pub os: Os,
    /// Every address the device was seen on.
    pub addresses: Vec<IpAddr>,
    /// TCP port the device receives transfers on.
    pub port: u16,
    /// Wire protocol version the device speaks.
    pub protocol_version: u16,
    /// Fingerprint of the device's certificate, as advertised. Unproven until a TLS handshake checks it.
    pub fingerprint: Fingerprint,
    /// When the device was last heard from.
    pub last_seen: Instant,
}

impl Peer {
    /// Addresses in the order to try them: private IPv4 first, then other IPv4, then IPv6.
    ///
    /// The sort is stable, so addresses in the same group keep the order they were found in.
    #[must_use]
    pub fn connect_order(&self) -> Vec<IpAddr> {
        let mut addresses = self.addresses.clone();
        addresses.sort_by_key(|address| address_rank(*address));
        addresses
    }

    /// [`Peer::connect_order`] combined with the peer's port, ready for
    /// [`transport::connect_any`](crate::transport::connect_any).
    #[must_use]
    pub fn socket_addrs(&self) -> Vec<SocketAddr> {
        self.connect_order().into_iter().map(|address| SocketAddr::new(address, self.port)).collect()
    }
}

/// Lower is tried first.
fn address_rank(address: IpAddr) -> u8 {
    match address {
        IpAddr::V4(v4) if v4.is_private() => 0,
        IpAddr::V4(_) => 1,
        IpAddr::V6(_) => 2,
    }
}

/// Whether an address can be connected to. Link-local IPv6 addresses are skipped: they need an interface scope id,
/// which `IpAddr` cannot carry.
#[must_use]
pub(crate) fn is_usable_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => !(v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast()),
        IpAddr::V6(v6) => !(v6.is_unspecified() || v6.is_multicast() || v6.is_unicast_link_local()),
    }
}

/// A change in the set of known peers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerEvent {
    /// A new device appeared.
    Found(Peer),
    /// A known device changed its name, OS, port, version or addresses.
    Updated(Peer),
    /// A device left or was not heard from for too long.
    Lost(DeviceId),
}

/// All peers seen recently, keyed by device id. This device's own id is always ignored.
#[derive(Debug)]
pub struct PeerTable {
    own_id: DeviceId,
    peers: HashMap<DeviceId, Peer>,
}

impl PeerTable {
    /// Creates an empty table that ignores `own_id`.
    #[must_use]
    pub fn new(own_id: DeviceId) -> Self {
        Self { own_id, peers: HashMap::new() }
    }

    /// Adds or refreshes a peer.
    ///
    /// Addresses are merged with the known ones, because mDNS and UDP broadcast each see only some of them.
    /// Returns `Found` for a new peer, `Updated` if anything visible changed, or `None` if it only refreshed
    /// `last_seen` (or the peer is this device).
    pub fn upsert(&mut self, mut peer: Peer) -> Option<PeerEvent> {
        if peer.id == self.own_id {
            return None;
        }
        peer.addresses.retain(|address| is_usable_address(*address));
        // `entry` looks the key up once and gives either the existing value or a slot to insert into.
        let existing = match self.peers.entry(peer.id) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                return Some(PeerEvent::Found(slot.insert(peer).clone()));
            }
            std::collections::hash_map::Entry::Occupied(slot) => slot.into_mut(),
        };

        let mut changed = existing.name != peer.name
            || existing.os != peer.os
            || existing.port != peer.port
            || existing.protocol_version != peer.protocol_version
            || existing.fingerprint != peer.fingerprint;
        for address in peer.addresses {
            if !existing.addresses.contains(&address) {
                existing.addresses.push(address);
                changed = true;
            }
        }
        existing.name = peer.name;
        existing.os = peer.os;
        existing.port = peer.port;
        existing.protocol_version = peer.protocol_version;
        existing.fingerprint = peer.fingerprint;
        existing.last_seen = existing.last_seen.max(peer.last_seen);
        changed.then(|| PeerEvent::Updated(existing.clone()))
    }

    /// Removes a peer that said goodbye. Returns `Lost` if it was known.
    pub fn remove(&mut self, id: DeviceId) -> Option<PeerEvent> {
        self.peers.remove(&id).map(|_| PeerEvent::Lost(id))
    }

    /// Removes every peer last seen before `older_than` and returns a `Lost` event for each.
    pub fn expire(&mut self, older_than: Instant) -> Vec<PeerEvent> {
        let expired: Vec<DeviceId> =
            self.peers.values().filter(|peer| peer.last_seen < older_than).map(|peer| peer.id).collect();
        for id in &expired {
            self.peers.remove(id);
        }
        expired.into_iter().map(PeerEvent::Lost).collect()
    }

    /// All known peers, sorted by name (case-insensitive), then id.
    #[must_use]
    pub fn peers(&self) -> Vec<Peer> {
        let mut peers: Vec<Peer> = self.peers.values().cloned().collect();
        peers.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
        peers
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{Ipv4Addr, Ipv6Addr},
        time::Duration,
    };

    use super::*;

    fn peer(id: DeviceId, name: &str, addresses: &[IpAddr], last_seen: Instant) -> Peer {
        Peer {
            id,
            name: name.to_owned(),
            os: Os::Linux,
            addresses: addresses.to_vec(),
            port: 52000,
            protocol_version: 2,
            fingerprint: Fingerprint::from_bytes([1; 32]),
            last_seen,
        }
    }

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn new_peer_is_found() {
        let mut table = PeerTable::new(DeviceId::new_random());
        let id = DeviceId::new_random();
        let event = table.upsert(peer(id, "laptop", &[v4(192, 168, 1, 5)], Instant::now()));
        assert!(matches!(event, Some(PeerEvent::Found(found)) if found.id == id));
        assert_eq!(table.peers().len(), 1);
    }

    #[test]
    fn own_id_is_ignored() {
        let own_id = DeviceId::new_random();
        let mut table = PeerTable::new(own_id);
        assert_eq!(table.upsert(peer(own_id, "me", &[v4(192, 168, 1, 2)], Instant::now())), None);
        assert_eq!(table.peers().len(), 0);
    }

    #[test]
    fn refresh_without_changes_emits_nothing_but_updates_last_seen() {
        let mut table = PeerTable::new(DeviceId::new_random());
        let id = DeviceId::new_random();
        let start = Instant::now();
        table.upsert(peer(id, "laptop", &[v4(192, 168, 1, 5)], start));
        let later = start + Duration::from_secs(10);
        assert_eq!(table.upsert(peer(id, "laptop", &[v4(192, 168, 1, 5)], later)), None);
        assert_eq!(table.peers()[0].last_seen, later);
    }

    #[test]
    fn changed_name_is_updated() {
        let mut table = PeerTable::new(DeviceId::new_random());
        let id = DeviceId::new_random();
        let now = Instant::now();
        table.upsert(peer(id, "laptop", &[v4(192, 168, 1, 5)], now));
        let event = table.upsert(peer(id, "work laptop", &[v4(192, 168, 1, 5)], now));
        assert!(matches!(event, Some(PeerEvent::Updated(updated)) if updated.name == "work laptop"));
    }

    #[test]
    fn addresses_are_merged() {
        let mut table = PeerTable::new(DeviceId::new_random());
        let id = DeviceId::new_random();
        let now = Instant::now();
        table.upsert(peer(id, "laptop", &[v4(192, 168, 1, 5)], now));
        let event = table.upsert(peer(id, "laptop", &[v4(10, 0, 0, 7)], now));
        assert!(matches!(event, Some(PeerEvent::Updated(_))));
        assert_eq!(table.peers()[0].addresses, vec![v4(192, 168, 1, 5), v4(10, 0, 0, 7)]);
    }

    #[test]
    fn unusable_addresses_are_dropped() {
        let mut table = PeerTable::new(DeviceId::new_random());
        let link_local_v6 = IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1));
        let addresses = [v4(0, 0, 0, 0), v4(255, 255, 255, 255), v4(224, 0, 0, 251), link_local_v6, v4(10, 0, 0, 1)];
        table.upsert(peer(DeviceId::new_random(), "laptop", &addresses, Instant::now()));
        assert_eq!(table.peers()[0].addresses, vec![v4(10, 0, 0, 1)]);
    }

    #[test]
    fn stale_peers_expire() {
        let mut table = PeerTable::new(DeviceId::new_random());
        let start = Instant::now();
        let old = DeviceId::new_random();
        let fresh = DeviceId::new_random();
        table.upsert(peer(old, "old", &[], start));
        table.upsert(peer(fresh, "fresh", &[], start + Duration::from_secs(20)));
        let events = table.expire(start + Duration::from_secs(5));
        assert_eq!(events, vec![PeerEvent::Lost(old)]);
        assert_eq!(table.peers().len(), 1);
        assert_eq!(table.peers()[0].id, fresh);
    }

    #[test]
    fn removed_peer_is_lost_once() {
        let mut table = PeerTable::new(DeviceId::new_random());
        let id = DeviceId::new_random();
        table.upsert(peer(id, "laptop", &[], Instant::now()));
        assert_eq!(table.remove(id), Some(PeerEvent::Lost(id)));
        assert_eq!(table.remove(id), None);
    }

    #[test]
    fn peers_are_sorted_by_name() {
        let mut table = PeerTable::new(DeviceId::new_random());
        let now = Instant::now();
        table.upsert(peer(DeviceId::new_random(), "beta", &[], now));
        table.upsert(peer(DeviceId::new_random(), "Alpha", &[], now));
        let names: Vec<String> = table.peers().into_iter().map(|peer| peer.name).collect();
        assert_eq!(names, ["Alpha", "beta"]);
    }

    #[test]
    fn connect_order_prefers_private_ipv4_then_ipv4_then_ipv6() {
        let global_v6 = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
        let addresses = [global_v6, v4(100, 64, 0, 1), v4(192, 168, 1, 5), v4(8, 8, 8, 8), v4(10, 0, 0, 2)];
        let peer = peer(DeviceId::new_random(), "laptop", &addresses, Instant::now());
        assert_eq!(
            peer.connect_order(),
            vec![v4(192, 168, 1, 5), v4(10, 0, 0, 2), v4(100, 64, 0, 1), v4(8, 8, 8, 8), global_v6]
        );
        assert_eq!(peer.socket_addrs()[0], SocketAddr::new(v4(192, 168, 1, 5), 52000));
    }

    #[test]
    fn connect_order_of_no_addresses_is_empty() {
        assert_eq!(peer(DeviceId::new_random(), "laptop", &[], Instant::now()).connect_order().len(), 0);
    }
}
