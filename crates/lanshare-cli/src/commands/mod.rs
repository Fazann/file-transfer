//! One module per subcommand, plus the dispatcher.

mod bench;
mod devices;
mod pair;
mod receive;
mod send;
mod trusted;
mod whoami;

use lanshare_core::{
    config::Config,
    discovery::DiscoveryOptions,
    identity::{DeviceCertificate, DeviceInfo, TrustStore},
};

use crate::args::{Cli, Command};

/// What every command loads first: settings, this device's certificate, and the paired devices.
pub struct Context {
    config: Config,
    certificate: DeviceCertificate,
    trust: TrustStore,
    use_mdns: bool,
}

impl Context {
    async fn load(use_mdns: bool) -> anyhow::Result<Self> {
        Ok(Self {
            config: Config::load().await?,
            certificate: DeviceCertificate::load_or_create().await?,
            trust: TrustStore::open().await?,
            use_mdns,
        })
    }

    /// This device as it introduces itself, with its certificate fingerprint.
    fn device_info(&self) -> DeviceInfo {
        self.config.device_info(self.certificate.fingerprint())
    }

    /// Discovery settings for a command. Only `receive` advertises; the others just listen.
    fn discovery_options(&self, advertise: bool) -> DiscoveryOptions {
        DiscoveryOptions {
            advertise,
            mdns: self.use_mdns,
            udp_fallback: self.config.udp_fallback,
            ..DiscoveryOptions::default()
        }
    }
}

/// Runs the chosen subcommand.
pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let context = Context::load(!cli.no_mdns).await?;
    match cli.command {
        Command::Receive(args) => receive::run(args, &context).await,
        Command::Send(args) => send::run(args, &context).await,
        Command::Devices => devices::run(&context).await,
        Command::Bench(args) => bench::run(args, &context).await,
        Command::Pair(args) => pair::run(args, &context).await,
        Command::Trusted(command) => trusted::run(command, &context).await,
        Command::Whoami => {
            whoami::run(&context);
            Ok(())
        }
    }
}
