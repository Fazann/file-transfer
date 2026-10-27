//! `lanshare receive`: advertises this device and waits for incoming files, one transfer at a time.

use std::path::Path;

use lanshare_core::{
    config::Config,
    discovery::Discovery,
    transfer::{cleanup_stale_parts, progress_channel, receive_file, IncomingOffer, OfferDecider, ReceiveOutcome},
    transport,
};
use tokio::net::TcpListener;

use super::discovery_options;
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

/// Listens until Ctrl+C, handling one connection at a time so prompts never overlap.
///
/// A failed transfer is reported and the loop keeps going; only listener errors end the command. On Ctrl+C,
/// discovery says goodbye so other devices drop this one right away.
pub async fn run(args: ReceiveArgs, config: &Config, use_mdns: bool) -> anyhow::Result<()> {
    let dir = args.dir.unwrap_or_else(|| config.download_dir.clone());
    tokio::fs::create_dir_all(&dir).await?;
    if let Err(error) = cleanup_stale_parts(&dir, config.stale_part_age()).await {
        tracing::warn!(%error, "could not clean up old unfinished transfers");
    }
    let listener = transport::listen(args.port.unwrap_or(config.port)).await?;

    let mut info = config.device_info();
    info.port = listener.local_addr()?.port();
    let (discovery, _events) = Discovery::start(&info, discovery_options(config, true, use_mdns))?;
    output::print_listening(&info.name, info.port, &dir);

    let decider = PromptDecider { auto_accept: args.yes };
    // `pin!` lets the same Ctrl+C future be polled again on every loop iteration.
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    let result = loop {
        tokio::select! {
            result = receive_one(&listener, &dir, &decider) => {
                if let Err(error) = result {
                    break Err(error);
                }
            }
            _ = &mut ctrl_c => break Ok(()),
        }
    };

    discovery.shutdown().await;
    output::print_stopped();
    result
}

/// Accepts one connection and handles its transfer.
async fn receive_one(listener: &TcpListener, dir: &Path, decider: &PromptDecider) -> anyhow::Result<()> {
    let (mut stream, peer_addr) = listener.accept().await?;
    tracing::info!(%peer_addr, "incoming connection");

    let (progress, watcher) = progress_channel();
    let bar = output::spawn_progress_bar(watcher);
    let result = receive_file(&mut stream, dir, decider, &progress).await;
    drop(progress);
    bar.await?;

    match result {
        Ok(ReceiveOutcome::Saved(received)) => output::print_saved(&received),
        Ok(ReceiveOutcome::Declined(offer)) => output::print_declined(&offer),
        Err(error) => output::print_receive_failed(&error),
    }
    Ok(())
}
