//! `lanshare devices`: lists the devices found on the network.

use std::time::Duration;

use lanshare_core::{config::Config, discovery::Discovery};

use super::discovery_options;
use crate::output;

/// How long to listen. Devices announce themselves at least every 5 seconds.
const LISTEN_TIME: Duration = Duration::from_secs(5);

/// Listens for [`LISTEN_TIME`] and prints every device found.
pub async fn run(config: &Config, use_mdns: bool) -> anyhow::Result<()> {
    let options = discovery_options(config, false, use_mdns);
    // The event channel is not needed: the full list is read once at the end.
    let (discovery, _events) = Discovery::start(&config.device_info(), options)?;
    output::print_searching(LISTEN_TIME);
    tokio::time::sleep(LISTEN_TIME).await;
    let peers = discovery.peers();
    discovery.shutdown().await;
    output::print_devices(&peers);
    Ok(())
}
