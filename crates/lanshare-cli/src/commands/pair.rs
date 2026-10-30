//! `lanshare pair <device-or-ip>`: pairs with a receiver. The receiver shows a PIN; the user types it here.

use lanshare_core::{
    identity::TrustedDevice,
    pairing::{self, PairingPeer, PinPrompt},
    transport::PeerCheck,
};

use super::{send::Destination, Context};
use crate::{args::PairArgs, output};

/// Asks for the PIN on the terminal.
struct TerminalPin;

impl PinPrompt for TerminalPin {
    async fn ask_pin(&self, receiver: &PairingPeer) -> Option<String> {
        let question = output::pin_question(receiver);
        // Reading stdin blocks the thread, so run it on tokio's blocking pool.
        tokio::task::spawn_blocking(move || output::ask_line(&question)).await.ok().flatten()
    }
}

/// Finds the device and pairs with it.
pub async fn run(args: PairArgs, context: &Context) -> anyhow::Result<()> {
    let destination = Destination::resolve(&args.target, args.port, args.transport.into(), context).await?;
    pair_with(&destination, context).await?;
    Ok(())
}

/// Connects in pairing mode, runs the PIN exchange and prints the result. Also used by `send` and `bench` when the
/// receiver is not paired yet.
pub(super) async fn pair_with(destination: &Destination, context: &Context) -> anyhow::Result<TrustedDevice> {
    let connection = destination.connect(context, PeerCheck::Pairing).await?;
    let device = pairing::pair(&connection, &context.device_info(), &context.trust, &TerminalPin).await?;
    output::print_paired(&device);
    Ok(device)
}
