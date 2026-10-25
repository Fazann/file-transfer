//! `lanshare send <device-or-ip> <file>`: sends one file to a receiver found by name or IP address.

use std::net::{IpAddr, SocketAddr};

use anyhow::bail;
use lanshare_core::{
    config::Config,
    discovery::{Discovery, DiscoveryOptions, Peer, PeerEvent},
    transfer::{progress_channel, send_file},
    transport::{self, PEER_ADDRESS_TIMEOUT},
};
use tokio::{
    net::TcpStream,
    time::{timeout_at, Duration, Instant},
};

use super::discovery_options;
use crate::{args::SendArgs, output};

/// How long to look for a device by name.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// After the first device with the right name shows up, wait this much longer to spot others with the same name.
const DUPLICATE_GRACE: Duration = Duration::from_secs(2);

/// Finds the receiver, connects, and sends the file with a progress bar.
pub async fn run(args: SendArgs, config: &Config, use_mdns: bool) -> anyhow::Result<()> {
    let local = config.device_info();
    let mut stream = connect(&args, config, use_mdns).await?;

    let (progress, watcher) = progress_channel();
    let bar = output::spawn_progress_bar(watcher);
    let result = send_file(&mut stream, &args.file, &local, &progress).await;
    // Dropping the sender tells the progress bar task to finish.
    drop(progress);
    bar.await?;

    result?;
    output::print_sent(&args.file);
    Ok(())
}

/// An IP address is used directly; anything else is looked up as a device name.
async fn connect(args: &SendArgs, config: &Config, use_mdns: bool) -> anyhow::Result<TcpStream> {
    if let Ok(ip) = args.target.parse::<IpAddr>() {
        return Ok(transport::connect(SocketAddr::new(ip, args.port)).await?);
    }
    let peer = find_by_name(&args.target, config, discovery_options(config, false, use_mdns)).await?;
    output::print_connecting(&peer);
    Ok(transport::connect_any(&peer.socket_addrs(), PEER_ADDRESS_TIMEOUT).await?)
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
