//! `lanshare receive`: runs the same service as the desktop app and answers its questions on the terminal: offers
//! (unless `--yes`), pairing PINs, and progress for every incoming transfer. Texts from paired devices are printed.

use std::sync::Arc;

use lanshare_core::{
    service::{Direction, LanShareService, ServiceEvent, ServiceOptions},
    transfer::IncomingOffer,
};
use tokio::sync::Mutex;

use super::Context;
use crate::{args::ReceiveArgs, output};

/// Receives until Ctrl+C. On Ctrl+C, running transfers stop (their partial files are kept) and discovery says
/// goodbye so other devices drop this one right away.
pub async fn run(args: ReceiveArgs, context: &Context) -> anyhow::Result<()> {
    let options = ServiceOptions {
        config: context.config.clone(),
        certificate: context.certificate.clone(),
        trust: context.trust.clone(),
        mdns: context.use_mdns,
        port: args.port,
        download_dir: args.dir,
        // The terminal has no clipboard to put texts on, so other devices' clipboards are declined.
        clipboard: false,
    };
    let (service, mut events) = LanShareService::start(options).await?;
    if !service.quic_enabled() {
        output::print_quic_unavailable(service.local().port);
    }
    output::print_listening(service.local(), service.download_dir());

    let mut bars = output::TransferBars::new();
    // Only one question on the terminal at a time, even when several offers arrive together.
    let terminal = Arc::new(Mutex::new(()));
    // `pin!` lets the same Ctrl+C future be polled again on every loop iteration.
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(event) => handle(event, &service, &mut bars, &terminal, args.yes),
                None => break,
            },
            _ = &mut ctrl_c => break,
        }
    }
    service.shutdown().await;
    output::print_stopped();
    Ok(())
}

fn handle(
    event: ServiceEvent,
    service: &LanShareService,
    bars: &mut output::TransferBars,
    terminal: &Arc<Mutex<()>>,
    auto_accept: bool,
) {
    match event {
        ServiceEvent::IncomingOffer { request_id, offer } if auto_accept => {
            // Answered at once, so it is still waiting.
            let _ = service.respond_to_offer(request_id, true);
            tracing::info!(file_name = %offer.file_name, "accepted with --yes");
        }
        ServiceEvent::IncomingOffer { request_id, offer } => {
            let (service, terminal) = (service.clone(), Arc::clone(terminal));
            tokio::spawn(async move {
                let _turn = terminal.lock().await;
                let accept = ask(&offer).await;
                if !service.respond_to_offer(request_id, accept) {
                    output::print_offer_expired(&offer);
                } else if !accept {
                    output::print_declined(&offer);
                }
            });
        }
        ServiceEvent::TransferStarted(info) if info.direction == Direction::Receive => bars.start(info),
        ServiceEvent::TransferProgress { id, progress, .. } => bars.update(id, progress),
        ServiceEvent::TransferComplete { id, path } => bars.complete(id, path.as_deref()),
        ServiceEvent::TransferFailed { id, error, .. } => bars.fail(id, &error),
        ServiceEvent::ReceiveRefused { error } => output::print_receive_refused(&error),
        ServiceEvent::TextReceived(text) => output::print_text_received(&text),
        ServiceEvent::PairingPin { peer, pin, .. } => output::print_pairing_pin(&peer, &pin),
        ServiceEvent::PairingResult { result: Ok(device), .. } => output::print_paired(&device),
        ServiceEvent::PairingResult { result: Err(error), .. } => output::print_pairing_failed(&error),
        ServiceEvent::Peer(_)
        | ServiceEvent::OfferClosed { .. }
        | ServiceEvent::TransferStarted(_)
        | ServiceEvent::ClipboardReceived(_) => {}
    }
}

/// Asks on the terminal. Reading stdin blocks the thread, so it runs on tokio's blocking pool.
async fn ask(offer: &IncomingOffer) -> bool {
    let question = output::offer_question(offer);
    tokio::task::spawn_blocking(move || output::ask_yes_no(&question)).await.unwrap_or(false)
}
