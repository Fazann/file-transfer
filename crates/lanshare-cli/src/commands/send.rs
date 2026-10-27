//! `lanshare send <device-or-ip> <file>`: sends one file to a receiver found by name or IP address, reconnecting and
//! resuming if the connection drops.

use std::net::{IpAddr, SocketAddr};

use anyhow::bail;
use lanshare_core::{
    config::Config,
    discovery::{Discovery, DiscoveryOptions, Peer, PeerEvent},
    paths,
    transfer::{progress_channel, send_file, SendOptions, TransferControl},
    transport::{self, AnyConnection, TransportPreference},
};
use tokio::time::{timeout_at, Duration, Instant};

use super::discovery_options;
use crate::{args::SendArgs, output};

/// How long to look for a device by name.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// After the first device with the right name shows up, wait this much longer to spot others with the same name.
const DUPLICATE_GRACE: Duration = Duration::from_secs(2);

/// Finds the receiver and sends the file with a progress bar.
pub async fn run(args: SendArgs, config: &Config, use_mdns: bool) -> anyhow::Result<()> {
    let local = config.device_info();
    let destination = Destination::resolve(&args.target, args.port, args.transport.into(), config, use_mdns).await?;
    // `paths` creates the directory with blocking `std::fs`, so it runs on the blocking pool.
    let mut options = SendOptions::new(tokio::task::spawn_blocking(paths::hash_cache_dir).await??);
    options.streams = args.streams.map_or(config.streams, usize::from);
    let control = TransferControl::new(args.limit.unwrap_or(config.bandwidth_limit_mbps));

    let (progress, watcher) = progress_channel();
    let bar = output::spawn_progress_bar(watcher);
    // The closure is called again for every reconnect, always trying the same addresses.
    let connect = || async {
        let connection = destination.connect().await?;
        output::print_transport(connection.kind());
        Ok(connection)
    };
    let result = send_file(connect, &args.file, &local, &options, &control, &progress).await;
    // Dropping the sender tells the progress bar task to finish.
    drop(progress);
    bar.await?;

    result?;
    output::print_sent(&args.file);
    Ok(())
}

/// Where to connect: one address typed by the user, or the addresses of a discovered device.
pub(super) struct Destination {
    addrs: Vec<SocketAddr>,
    is_typed_address: bool,
    preference: TransportPreference,
}

impl Destination {
    /// An IP address is used directly; anything else is looked up as a device name.
    pub(super) async fn resolve(
        target: &str,
        port: u16,
        preference: TransportPreference,
        config: &Config,
        use_mdns: bool,
    ) -> anyhow::Result<Self> {
        if let Ok(ip) = target.parse::<IpAddr>() {
            return Ok(Self { addrs: vec![SocketAddr::new(ip, port)], is_typed_address: true, preference });
        }
        let peer = find_by_name(target, config, discovery_options(config, false, use_mdns)).await?;
        output::print_connecting(&peer);
        Ok(Self { addrs: peer.socket_addrs(), is_typed_address: false, preference })
    }

    pub(super) async fn connect(&self) -> lanshare_core::Result<AnyConnection> {
        match self.addrs.as_slice() {
            [addr] if self.is_typed_address => transport::connect(*addr, self.preference).await,
            addrs => transport::connect_any(addrs, self.preference).await,
        }
    }
}

/// Looks for exactly one device whose name matches `name`, ignoring case.
async fn find_by_name(name: &str, config: &Config, options: DiscoveryOptions) -> anyhow::Result<Peer> {
    let (discovery, mut events) = Discovery::start(&config.device_info(), options)?;
    let wanted = name.to_lowercase();
    let is_match = |peer: &Peer| peer.name.to_lowercase() == wanted;

    let deadline = Instant::now() + LOOKUP_TIMEOUT;
    let mut stop_at = deadline;
    // `timeout_at` returns `Err` once `stop_at` passes; `recv` returns `None` if discovery stopped.
    while let Ok(Some(event)) = timeout_at(stop_at, events.recv()).await {
        if let PeerEvent::Found(peer) | PeerEvent::Updated(peer) = event {
            if is_match(&peer) {
                stop_at = stop_at.min(Instant::now() + DUPLICATE_GRACE);
            }
        }
    }
    let mut matches: Vec<Peer> = discovery.peers().into_iter().filter(|peer| is_match(peer)).collect();
    discovery.shutdown().await;

    match matches.len() {
        0 => bail!(output::no_device_named(name)),
        1 => Ok(matches.remove(0)),
        _ => bail!(output::ambiguous_device_name(name, &matches)),
    }
}
