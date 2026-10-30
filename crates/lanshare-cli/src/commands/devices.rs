//! `lanshare devices`: lists the devices found on the network.

use std::time::Duration;

use lanshare_core::discovery::Discovery;

use super::Context;
use crate::output;

/// How long to listen. Devices announce themselves at least every 5 seconds.
const LISTEN_TIME: Duration = Duration::from_secs(5);

/// Listens for [`LISTEN_TIME`] and prints every device found, marking the paired ones.
pub async fn run(context: &Context) -> anyhow::Result<()> {
    // The event channel is not needed: the full list is read once at the end.
    let (discovery, _events) = Discovery::start(&context.device_info(), context.discovery_options(false))?;
    output::print_searching(LISTEN_TIME);
    tokio::time::sleep(LISTEN_TIME).await;
    let peers = discovery.peers();
    discovery.shutdown().await;
    output::print_devices(&peers, |peer| context.trust.get_by_fingerprint(&peer.fingerprint).is_some());
    Ok(())
}
