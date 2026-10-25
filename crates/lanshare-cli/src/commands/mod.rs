//! One module per subcommand, plus the dispatcher.

mod devices;
mod receive;
mod send;

use lanshare_core::{config::Config, discovery::DiscoveryOptions};

use crate::args::{Cli, Command};

/// Runs the chosen subcommand.
pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let config = Config::load().await?;
    match cli.command {
        Command::Receive(args) => receive::run(args, &config, !cli.no_mdns).await,
        Command::Send(args) => send::run(args, &config, !cli.no_mdns).await,
        Command::Devices => devices::run(&config, !cli.no_mdns).await,
    }
}

/// Discovery settings for a command. Only `receive` advertises; the others just listen.
fn discovery_options(config: &Config, advertise: bool, use_mdns: bool) -> DiscoveryOptions {
    DiscoveryOptions { advertise, mdns: use_mdns, udp_fallback: config.udp_fallback, ..DiscoveryOptions::default() }
}
