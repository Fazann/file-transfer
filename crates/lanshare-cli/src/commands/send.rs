//! `lanshare send <device-or-ip> <files...>`: sends files and folders in one transfer to a receiver found by name or
//! IP address, reconnecting and resuming if the connection drops. If the two devices are not paired yet, pairs them
//! first.

use std::net::{IpAddr, SocketAddr};

use anyhow::bail;
use lanshare_core::{
    discovery::{Discovery, Peer, PeerEvent},
    identity::Fingerprint,
    paths,
    transfer::{progress_channel, send_files, SendOptions, TransferControl},
    transport::{self, AnyConnection, ConnectOptions, PeerCheck, TransportPreference},
    Error,
};
use tokio::time::{timeout_at, Duration, Instant};

use super::{pair, Context};
use crate::{args::SendArgs, output};

/// How long to look for a device by name.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// After the first device with the right name shows up, wait this much longer to spot others with the same name.
const DUPLICATE_GRACE: Duration = Duration::from_secs(2);

/// Finds the receiver and sends the file with a progress bar, pairing first if needed.
pub async fn run(args: SendArgs, context: &Context) -> anyhow::Result<()> {
    let destination = Destination::resolve(&args.target, args.port, args.transport.into(), context).await?;
    // `paths` creates the directory with blocking `std::fs`, so it runs on the blocking pool.
    let mut options = SendOptions::new(tokio::task::spawn_blocking(paths::hash_cache_dir).await??);
    options.streams = args.streams.map_or(context.config.streams, usize::from);
    let control = TransferControl::new(args.limit.unwrap_or(context.config.bandwidth_limit_mbps));

    match send_once(&args, context, &destination, &options, &control).await {
        // The receiver is not in our trust store, or we are not in its: pairing fixes both.
        Err(Error::UntrustedDevice) => {
            output::print_pairing_needed();
            pair::pair_with(&destination, context).await?;
            send_once(&args, context, &destination, &options, &control).await?;
        }
        result => result?,
    }
    output::print_sent(&args.files);
    Ok(())
}

/// One `send_file` run with its own progress bar.
async fn send_once(
    args: &SendArgs,
    context: &Context,
    destination: &Destination,
    options: &SendOptions,
    control: &TransferControl,
) -> lanshare_core::Result<()> {
    let (progress, watcher) = progress_channel();
    let bar = output::spawn_progress_bar(watcher);
    // The closure is called again for every reconnect, always trying the same addresses.
    let connect = || async {
        let connection = destination.connect(context, PeerCheck::Trusted(context.trust.clone())).await?;
        output::print_transport(connection.kind());
        Ok(connection)
    };
    let result = send_files(connect, &args.files, &context.device_info(), options, control, &progress).await;
    // Dropping the sender tells the progress bar task to finish.
    drop(progress);
    let _ = bar.await;
    result
}

/// Where to connect: one address typed by the user, or the addresses of a discovered device.
pub(super) struct Destination {
    addrs: Vec<SocketAddr>,
    is_typed_address: bool,
    preference: TransportPreference,
    /// The fingerprint a discovered device advertised. A typed address has none to compare with.
    expected: Option<Fingerprint>,
}

impl Destination {
    /// An IP address is used directly; anything else is looked up as a device name.
    pub(super) async fn resolve(
        target: &str,
        port: u16,
        preference: TransportPreference,
        context: &Context,
    ) -> anyhow::Result<Self> {
        if let Ok(ip) = target.parse::<IpAddr>() {
            let addrs = vec![SocketAddr::new(ip, port)];
            return Ok(Self { addrs, is_typed_address: true, preference, expected: None });
        }
        let peer = find_by_name(target, context).await?;
        output::print_connecting(&peer, context.trust.get_by_fingerprint(&peer.fingerprint).is_some());
        Ok(Self { addrs: peer.socket_addrs(), is_typed_address: false, preference, expected: Some(peer.fingerprint) })
    }

    /// Connects, checking the receiver's certificate as `check` says.
    pub(super) async fn connect(&self, context: &Context, check: PeerCheck) -> lanshare_core::Result<AnyConnection> {
        let options = ConnectOptions {
            preference: self.preference,
            certificate: context.certificate.clone(),
            check,
            expected: self.expected,
        };
        match self.addrs.as_slice() {
            [addr] if self.is_typed_address => transport::connect(*addr, &options).await,
            addrs => transport::connect_any(addrs, &options).await,
        }
    }
}

/// Looks for exactly one device whose name matches `name`, ignoring case.
async fn find_by_name(name: &str, context: &Context) -> anyhow::Result<Peer> {
    let (discovery, mut events) = Discovery::start(&context.device_info(), context.discovery_options(false))?;
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
