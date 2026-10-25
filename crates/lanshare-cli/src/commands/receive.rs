//! `lanshare receive`: waits for incoming files, one transfer at a time.

use lanshare_core::{
    transfer::{progress_channel, receive_file, IncomingOffer, OfferDecider, ReceiveOutcome},
    transport,
};

use crate::{args::ReceiveArgs, output};

/// Asks on the terminal before accepting, unless `--yes` was given.
struct PromptDecider {
    auto_accept: bool,
}

impl OfferDecider for PromptDecider {
    async fn decide(&self, offer: &IncomingOffer) -> bool {
        if self.auto_accept {
            return true;
        }
        let question = output::offer_question(offer);
        // Reading stdin blocks the thread, so run it on tokio's blocking pool.
        tokio::task::spawn_blocking(move || output::ask_yes_no(&question)).await.unwrap_or(false)
    }
}

/// Listens forever, handling one connection at a time so prompts never overlap.
///
/// A failed transfer is reported and the loop keeps going; only listener errors end the command.
pub async fn run(args: ReceiveArgs) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(&args.dir).await?;
    let listener = transport::listen(args.port).await?;
    let decider = PromptDecider { auto_accept: args.yes };
    output::print_listening(args.port, &args.dir);

    loop {
        let (mut stream, peer_addr) = listener.accept().await?;
        tracing::info!(%peer_addr, "incoming connection");

        let (progress, watcher) = progress_channel();
        let bar = output::spawn_progress_bar(watcher);
        let result = receive_file(&mut stream, &args.dir, &decider, &progress).await;
        drop(progress);
        bar.await?;

        match result {
            Ok(ReceiveOutcome::Saved(received)) => output::print_saved(&received),
            Ok(ReceiveOutcome::Declined(offer)) => output::print_declined(&offer),
            Err(error) => output::print_receive_failed(&error),
        }
    }
}
