//! Two full services on localhost: discovery, pairing through events, an accepted transfer, and cancelling.

// `allow-unwrap-in-tests` in clippy.toml only covers `#[test]` functions, not the shared helpers in this file.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use lanshare_core::{
    config::Config,
    discovery::PeerEvent,
    identity::{DeviceCertificate, TrustStore, TRUST_FILE_NAME},
    service::{Direction, LanShareService, ServiceEvent, ServiceOptions},
    ErrorCode,
};
use tempfile::TempDir;
use tokio::sync::mpsc::UnboundedReceiver;

/// Long enough for discovery on a busy test machine.
const WAIT: Duration = Duration::from_secs(20);

struct Node {
    service: LanShareService,
    events: UnboundedReceiver<ServiceEvent>,
    dir: TempDir,
}

impl Node {
    async fn start(name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            device_name: name.into(),
            port: 0,
            download_dir: dir.path().join("downloads"),
            bandwidth_limit_mbps: 0,
            ..Config::default()
        };
        let options = ServiceOptions {
            config,
            certificate: DeviceCertificate::generate().unwrap(),
            trust: TrustStore::open_at(&dir.path().join(TRUST_FILE_NAME)).unwrap(),
            mdns: true,
            port: None,
            download_dir: None,
        };
        let (service, events) = LanShareService::start(options).await.unwrap();
        Self { service, events, dir }
    }

    /// Waits for the first event `pick` returns something for, skipping the others.
    async fn wait_for<T>(&mut self, mut pick: impl FnMut(ServiceEvent) -> Option<T>) -> T {
        tokio::time::timeout(WAIT, async {
            loop {
                if let Some(found) = pick(self.events.recv().await.unwrap()) {
                    return found;
                }
            }
        })
        .await
        .unwrap()
    }

    async fn wait_for_peer(&mut self, other: &Self) {
        let id = other.service.local().id;
        self.wait_for(|event| match event {
            ServiceEvent::Peer(PeerEvent::Found(peer) | PeerEvent::Updated(peer)) if peer.id == id => Some(()),
            _ => None,
        })
        .await;
    }
}

/// Pairs `sender` with `receiver` through the service API, as the app does.
async fn pair(sender: &mut Node, receiver: &mut Node) {
    let request = sender.service.start_pairing(receiver.service.local().id).unwrap();
    let pin = receiver
        .wait_for(|event| if let ServiceEvent::PairingPin { pin, .. } = event { Some(pin) } else { None })
        .await;
    // The pairing task asks for the PIN right after the receiver's hello; retry until it is waiting.
    tokio::time::timeout(WAIT, async {
        while !sender.service.submit_pin(request, Some(pin.clone())) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let result = sender
        .wait_for(|event| match event {
            ServiceEvent::PairingResult { request_id, result } if request_id == request => Some(result),
            _ => None,
        })
        .await;
    assert_eq!(result.unwrap().id, receiver.service.local().id);
    let result = receiver
        .wait_for(|event| if let ServiceEvent::PairingResult { result, .. } = event { Some(result) } else { None })
        .await;
    assert_eq!(result.unwrap().id, sender.service.local().id);
}

#[tokio::test(flavor = "multi_thread")]
async fn devices_pair_and_transfer_through_the_service() {
    let (mut sender, mut receiver) = (Node::start("Service Sender").await, Node::start("Service Receiver").await);
    sender.wait_for_peer(&receiver).await;
    pair(&mut sender, &mut receiver).await;

    let source = sender.dir.path().join("report.txt");
    std::fs::write(&source, b"quarterly numbers").unwrap();
    let sent = sender.service.send(receiver.service.local().id, vec![source]).unwrap();

    let (request, offer) = receiver
        .wait_for(|event| match event {
            ServiceEvent::IncomingOffer { request_id, offer } => Some((request_id, offer)),
            _ => None,
        })
        .await;
    assert_eq!((offer.file_name.as_str(), offer.trusted), ("report.txt", true));
    assert!(receiver.service.respond_to_offer(request, true));

    let started = receiver
        .wait_for(|event| if let ServiceEvent::TransferStarted(info) = event { Some(info) } else { None })
        .await;
    assert_eq!((started.direction, started.size), (Direction::Receive, 17));
    let path = receiver
        .wait_for(|event| match event {
            ServiceEvent::TransferComplete { id, path } if id == started.id => Some(path),
            ServiceEvent::TransferFailed { error, .. } => panic!("receive failed: {error:?}"),
            _ => None,
        })
        .await
        .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"quarterly numbers");
    sender
        .wait_for(|event| match event {
            ServiceEvent::TransferComplete { id, .. } if id == sent => Some(()),
            ServiceEvent::TransferFailed { error, .. } => panic!("send failed: {error:?}"),
            _ => None,
        })
        .await;
    sender.service.shutdown().await;
    receiver.service.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_download_folder_tells_both_devices() {
    use std::os::unix::fs::PermissionsExt;

    let (mut sender, mut receiver) = (Node::start("Refused Sender").await, Node::start("Refused Receiver").await);
    sender.wait_for_peer(&receiver).await;
    pair(&mut sender, &mut receiver).await;

    let downloads = receiver.dir.path().join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    std::fs::set_permissions(&downloads, std::fs::Permissions::from_mode(0o555)).unwrap();
    let source = sender.dir.path().join("report.txt");
    std::fs::write(&source, b"quarterly numbers").unwrap();
    let sent = sender.service.send(receiver.service.local().id, vec![source]).unwrap();

    let expected = ErrorCode::PermissionDenied { folder: downloads.display().to_string() };
    let refused = receiver
        .wait_for(|event| if let ServiceEvent::ReceiveRefused { error } = event { Some(error) } else { None })
        .await;
    assert_eq!(refused, expected);
    let failed = sender
        .wait_for(|event| match event {
            ServiceEvent::TransferFailed { id, error } if id == sent => Some(error),
            _ => None,
        })
        .await;
    assert_eq!(failed, expected);
    std::fs::set_permissions(&downloads, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_stops_both_sides() {
    let (mut sender, mut receiver) = (Node::start("Cancel Sender").await, Node::start("Cancel Receiver").await);
    sender.wait_for_peer(&receiver).await;
    pair(&mut sender, &mut receiver).await;
    let mut settings = Config { bandwidth_limit_mbps: 5, ..Config::default() };
    settings.streams = 1;
    sender.service.update_settings(settings);

    let source = sender.dir.path().join("big.bin");
    std::fs::write(&source, vec![7u8; 50_000_000]).unwrap();
    let id = sender.service.send(receiver.service.local().id, vec![source]).unwrap();
    let request = receiver
        .wait_for(
            |event| if let ServiceEvent::IncomingOffer { request_id, .. } = event { Some(request_id) } else { None },
        )
        .await;
    assert!(receiver.service.respond_to_offer(request, true));
    sender
        .wait_for(|event| match event {
            ServiceEvent::TransferProgress { id: progress_id, progress, .. } if progress_id == id => {
                (progress.transferred > 0).then_some(())
            }
            _ => None,
        })
        .await;

    assert!(sender.service.cancel(id));
    let error = sender
        .wait_for(|event| match event {
            ServiceEvent::TransferFailed { id: failed, error } if failed == id => Some(error),
            _ => None,
        })
        .await;
    assert_eq!(error, ErrorCode::Cancelled);
    let error = receiver
        .wait_for(|event| if let ServiceEvent::TransferFailed { error, .. } = event { Some(error) } else { None })
        .await;
    assert_eq!(error, ErrorCode::Cancelled, "the receiver is told instead of waiting for a reconnect");
    sender.service.shutdown().await;
    receiver.service.shutdown().await;
}
