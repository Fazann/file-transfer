//! Integration tests for phase 5: certificates, trust, PIN pairing, the connection limit, and encryption on the wire.
//! Every test makes its own devices with fresh certificates and empty trust stores.

// `allow-unwrap-in-tests` in clippy.toml only covers `#[test]` functions, not the shared helpers in this file.
#![allow(clippy::unwrap_used)]

use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use lanshare_core::{
    identity::{DeviceCertificate, DeviceId, DeviceInfo, Fingerprint, Os, TrustStore, TrustedDevice, TRUST_FILE_NAME},
    pairing::{self, PairingLockout, PairingPeer, PinPrompt, MAX_FAILURES},
    protocol::{read_message, write_message, Message, PROTOCOL_VERSION},
    transfer::{
        progress_channel, receive_file, send_clipboard, send_file, send_text, IncomingOffer, OfferDecider,
        ReceiveOutcome, ReceivePolicy, SendOptions, TransferControl,
    },
    transport::{
        self, AnyConnection, ConnectOptions, Connection, Listener, PeerCheck, TransportPreference,
        MAX_INCOMING_CONNECTIONS,
    },
    Error, ErrorCode,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinHandle,
};

/// Generates one `#[tokio::test]` per transport for a scenario `async fn(TransportPreference)`.
macro_rules! over_both_transports {
    ($scenario:ident: $tcp:ident, $quic:ident) => {
        #[tokio::test(flavor = "multi_thread")]
        async fn $tcp() {
            $scenario(TransportPreference::Tcp).await;
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn $quic() {
            $scenario(TransportPreference::Quic).await;
        }
    };
}

/// One device: its certificate, its trust store and how it introduces itself.
struct Device {
    certificate: DeviceCertificate,
    trust: TrustStore,
    info: DeviceInfo,
    dir: TempDir,
}

impl Device {
    fn new(name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let certificate = DeviceCertificate::generate().unwrap();
        let trust = TrustStore::open_at(&dir.path().join(TRUST_FILE_NAME)).unwrap();
        let info = DeviceInfo {
            id: DeviceId::new_random(),
            name: name.into(),
            os: Os::current(),
            port: 0,
            protocol_version: PROTOCOL_VERSION,
            fingerprint: certificate.fingerprint(),
        };
        Self { certificate, trust, info, dir }
    }

    /// A device that copies this one's id and name, but has its own key.
    fn impostor_of(original: &Self) -> Self {
        let mut impostor = Self::new(&original.info.name);
        impostor.info.id = original.info.id;
        impostor
    }

    /// Adds `other` to this device's trust store, as pairing would.
    async fn trust(&self, other: &Self) {
        let device = TrustedDevice::paired_now(other.info.id, other.info.name.clone(), other.certificate.fingerprint());
        self.trust.add(device).await.unwrap();
    }

    fn connect_options(&self, preference: TransportPreference, check: PeerCheck) -> ConnectOptions {
        ConnectOptions { preference, certificate: self.certificate.clone(), check, expected: None, peer_os: None }
    }

    fn trusted_check(&self) -> PeerCheck {
        PeerCheck::Trusted(self.trust.clone())
    }

    fn downloads(&self) -> PathBuf {
        let downloads = self.dir.path().join("downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        downloads
    }
}

/// The receiving user: accepts or declines offers, remembers being asked, and publishes the PIN it is shown.
struct Receiver {
    accept: bool,
    asked: AtomicBool,
    last_offer: Mutex<Option<IncomingOffer>>,
    pins: watch::Sender<Option<String>>,
}

impl Receiver {
    fn new(accept: bool) -> Arc<Self> {
        Arc::new(Self {
            accept,
            asked: AtomicBool::new(false),
            last_offer: Mutex::new(None),
            pins: watch::channel(None).0,
        })
    }
}

impl OfferDecider for Receiver {
    fn decide(&self, offer: &IncomingOffer) -> impl Future<Output = bool> + Send {
        self.asked.store(true, Ordering::SeqCst);
        *self.last_offer.lock().unwrap() = Some(offer.clone());
        std::future::ready(self.accept)
    }

    fn show_pin(&self, _peer: &PairingPeer, pin: &str) {
        self.pins.send_replace(Some(pin.to_owned()));
    }
}

/// The sending user: types the PIN the receiver shows, or a wrong one.
struct TypesPin {
    pins: watch::Receiver<Option<String>>,
    correct: bool,
}

impl PinPrompt for TypesPin {
    async fn ask_pin(&self, _receiver: &PairingPeer) -> Option<String> {
        let mut pins = self.pins.clone();
        let shown = pins.wait_for(Option::is_some).await.ok()?.clone()?;
        if self.correct {
            return Some(shown);
        }
        // Change the last digit, so the PIN is wrong but still six digits.
        let last = shown.chars().last()?.to_digit(10)?;
        Some(format!("{}{}", &shown[..5], (last + 1) % 10))
    }
}

/// How the receiving device treats senders.
#[derive(Debug, Clone, Copy, Default)]
struct Policy {
    allow_untrusted: bool,
    /// Like `lanshare receive`, which has no clipboard.
    no_clipboard: bool,
}

/// A listener for `device`, handling connections one at a time like `lanshare receive`.
struct Running {
    addr: SocketAddr,
    listener: Arc<Listener>,
    policy: ReceivePolicy,
    downloads: PathBuf,
}

impl Running {
    async fn start(device: &Device, policy: Policy) -> Self {
        let listener = Arc::new(Listener::bind(0, &device.certificate).await.unwrap());
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, listener.port()));
        let policy = ReceivePolicy {
            local: device.info.clone(),
            trust: device.trust.clone(),
            allow_untrusted: policy.allow_untrusted,
            accept_clipboard: !policy.no_clipboard,
            lockout: Arc::new(PairingLockout::new()),
        };
        Self { addr, listener, policy, downloads: device.downloads() }
    }

    /// Handles the next connection in the background.
    fn receive_one(&self, user: &Arc<Receiver>) -> JoinHandle<Result<ReceiveOutcome, Error>> {
        let (listener, policy, downloads, user) =
            (Arc::clone(&self.listener), self.policy.clone(), self.downloads.clone(), Arc::clone(user));
        tokio::spawn(async move {
            let connection = listener.accept().await.unwrap();
            let (progress, _watcher) = progress_channel();
            receive_file(&connection, &downloads, &*user, &policy, &TransferControl::default(), &progress).await
        })
    }
}

/// Sends a small file from `sender` to `receiver`, requiring the receiver to be trusted.
async fn send_small_file(sender: &Device, addr: SocketAddr, preference: TransportPreference) -> Result<(), Error> {
    let source = sender.dir.path().join("hello.txt");
    std::fs::write(&source, b"hello").unwrap();
    let options = SendOptions::new(sender.dir.path().join("hash-cache"));
    let connect_options = sender.connect_options(preference, sender.trusted_check());
    let connect = || transport::connect(addr, &connect_options);
    let (progress, _watcher) = progress_channel();
    send_file(connect, &source, &sender.info, &options, &TransferControl::default(), &progress).await
}

/// Runs one pairing attempt from `sender` to `receiver`, typing the right PIN or a wrong one.
async fn pair_once(
    sender: &Device,
    running: &Running,
    preference: TransportPreference,
    correct_pin: bool,
) -> (Result<TrustedDevice, Error>, Result<ReceiveOutcome, Error>) {
    let user = Receiver::new(true);
    let handled = running.receive_one(&user);
    let connection =
        transport::connect(running.addr, &sender.connect_options(preference, PeerCheck::Pairing)).await.unwrap();
    let prompt = TypesPin { pins: user.pins.subscribe(), correct: correct_pin };
    let paired = pairing::pair(&connection, &sender.info, &sender.trust, &prompt).await;
    (paired, handled.await.unwrap())
}

// ---------------------------------------------------------------------------------------------------------------
// Pairing.

async fn pairing_lets_devices_transfer_without_a_pin(preference: TransportPreference) {
    let (sender, receiver) = (Device::new("Laptop"), Device::new("Desk"));
    let running = Running::start(&receiver, Policy::default()).await;

    // Before pairing, the sender refuses to talk to the unknown receiver.
    let error = send_small_file(&sender, running.addr, preference).await.unwrap_err();
    assert_eq!(error.code(), ErrorCode::UntrustedDevice);

    let (paired, outcome) = pair_once(&sender, &running, preference, true).await;
    let paired = paired.unwrap();
    assert_eq!((paired.id, paired.name.as_str()), (receiver.info.id, "Desk"));
    assert_eq!(paired.fingerprint, receiver.certificate.fingerprint());
    let ReceiveOutcome::Paired(sender_entry) = outcome.unwrap() else { panic!("expected pairing") };
    assert_eq!(sender_entry.fingerprint, sender.certificate.fingerprint());
    assert!(!sender_entry.auto_accept, "auto-accept is off after pairing");
    assert!(receiver.trust.get_by_fingerprint(&sender.certificate.fingerprint()).is_some());
    assert!(sender.trust.get_by_fingerprint(&receiver.certificate.fingerprint()).is_some());

    // After pairing, a transfer needs no PIN; the receiving user is still asked.
    let user = Receiver::new(true);
    let handled = running.receive_one(&user);
    send_small_file(&sender, running.addr, preference).await.unwrap();
    assert!(matches!(handled.await.unwrap().unwrap(), ReceiveOutcome::Saved(_)));
    let offer = user.last_offer.lock().unwrap().clone().unwrap();
    assert!(offer.trusted);
    assert_eq!(offer.fingerprint, sender.certificate.fingerprint());
}
over_both_transports!(pairing_lets_devices_transfer_without_a_pin: pairing_over_tcp, pairing_over_quic);

#[tokio::test(flavor = "multi_thread")]
async fn wrong_pin_fails_and_repeated_failures_lock_pairing() {
    let (sender, receiver) = (Device::new("Guesser"), Device::new("Desk"));
    let running = Running::start(&receiver, Policy::default()).await;
    let preference = TransportPreference::Quic;

    for _ in 0..MAX_FAILURES {
        let (paired, outcome) = pair_once(&sender, &running, preference, false).await;
        assert_eq!(paired.unwrap_err().code(), ErrorCode::PairingFailed);
        assert_eq!(outcome.unwrap_err().code(), ErrorCode::PairingFailed);
    }
    assert!(receiver.trust.list().is_empty() && sender.trust.list().is_empty(), "nothing is trusted");

    // Locked now, even with the right PIN.
    let (paired, outcome) = pair_once(&sender, &running, preference, true).await;
    let Err(Error::PairingLocked { retry_after_secs }) = paired else { panic!("expected a lockout, got {paired:?}") };
    assert!((290..=301).contains(&retry_after_secs), "locked for about five minutes, got {retry_after_secs} s");
    assert!(matches!(outcome, Err(Error::PairingLocked { .. })));
}

// ---------------------------------------------------------------------------------------------------------------
// Trust checks.

async fn unpaired_sender_is_rejected(preference: TransportPreference) {
    let (sender, receiver) = (Device::new("Stranger"), Device::new("Desk"));
    // The sender trusts the receiver, but not the other way round.
    sender.trust(&receiver).await;
    let running = Running::start(&receiver, Policy::default()).await;

    let user = Receiver::new(true);
    let handled = running.receive_one(&user);
    let error = send_small_file(&sender, running.addr, preference).await.unwrap_err();
    assert_eq!(error.code(), ErrorCode::UntrustedDevice);
    assert_eq!(handled.await.unwrap().unwrap_err().code(), ErrorCode::UntrustedDevice);
    assert!(!user.asked.load(Ordering::SeqCst), "the user is never asked about unpaired devices");
}
over_both_transports!(unpaired_sender_is_rejected: unpaired_sender_over_tcp, unpaired_sender_over_quic);

#[tokio::test(flavor = "multi_thread")]
async fn allow_untrusted_asks_the_user() {
    let (sender, receiver) = (Device::new("Visitor"), Device::new("Desk"));
    sender.trust(&receiver).await;
    let running = Running::start(&receiver, Policy { allow_untrusted: true, ..Policy::default() }).await;

    let user = Receiver::new(true);
    let handled = running.receive_one(&user);
    send_small_file(&sender, running.addr, TransportPreference::Quic).await.unwrap();
    assert!(matches!(handled.await.unwrap().unwrap(), ReceiveOutcome::Saved(_)));
    assert!(!user.last_offer.lock().unwrap().clone().unwrap().trusted);
}

#[tokio::test(flavor = "multi_thread")]
async fn text_from_an_unpaired_device_is_rejected_even_with_allow_untrusted() {
    let (sender, receiver) = (Device::new("Visitor"), Device::new("Desk"));
    sender.trust(&receiver).await;
    let running = Running::start(&receiver, Policy { allow_untrusted: true, ..Policy::default() }).await;

    let user = Receiver::new(true);
    let handled = running.receive_one(&user);
    let options = sender.connect_options(TransportPreference::Quic, sender.trusted_check());
    let connection = transport::connect(running.addr, &options).await.unwrap();
    let error = send_text(&connection, &sender.info, "click this link").await.unwrap_err();
    assert_eq!(error.code(), ErrorCode::UntrustedDevice);
    assert_eq!(handled.await.unwrap().unwrap_err().code(), ErrorCode::UntrustedDevice);
    assert!(!user.asked.load(Ordering::SeqCst));
}

/// Sends `text` from `sender`'s clipboard to the receiver running at `running`, returning both sides' results.
async fn share_clipboard(
    sender: &Device,
    running: &Running,
    text: &str,
) -> (Result<(), Error>, Result<ReceiveOutcome, Error>) {
    let handled = running.receive_one(&Receiver::new(false));
    let options = sender.connect_options(TransportPreference::Quic, sender.trusted_check());
    let connection = transport::connect(running.addr, &options).await.unwrap();
    let sent = send_clipboard(&connection, &sender.info, text).await;
    (sent, handled.await.unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn clipboard_is_taken_only_once_the_receiver_syncs_with_the_sender() {
    let (sender, receiver) = (Device::new("Laptop"), Device::new("Desk"));
    sender.trust(&receiver).await;
    receiver.trust(&sender).await;
    let running = Running::start(&receiver, Policy::default()).await;

    // Paired, but the receiver has not turned clipboard sync on for this sender.
    let (sent, handled) = share_clipboard(&sender, &running, "copied").await;
    assert_eq!(sent.unwrap_err().code(), ErrorCode::Declined);
    assert_eq!(handled.unwrap_err().code(), ErrorCode::Declined);

    receiver.trust.set_sync_clipboard(sender.info.id, true).await.unwrap();
    let (sent, handled) = share_clipboard(&sender, &running, "copied").await;
    sent.unwrap();
    let ReceiveOutcome::Clipboard(got) = handled.unwrap() else { panic!("expected a clipboard text") };
    assert_eq!(got.text, "copied");
    assert_eq!(got.device_id, sender.info.id);
    assert_eq!(got.fingerprint, sender.certificate.fingerprint());
}

#[tokio::test(flavor = "multi_thread")]
async fn clipboard_is_declined_by_a_receiver_without_a_clipboard() {
    let (sender, receiver) = (Device::new("Laptop"), Device::new("Server"));
    sender.trust(&receiver).await;
    receiver.trust(&sender).await;
    receiver.trust.set_sync_clipboard(sender.info.id, true).await.unwrap();
    let running = Running::start(&receiver, Policy { no_clipboard: true, ..Policy::default() }).await;

    let (sent, handled) = share_clipboard(&sender, &running, "copied").await;
    assert_eq!(sent.unwrap_err().code(), ErrorCode::Declined);
    assert_eq!(handled.unwrap_err().code(), ErrorCode::Declined);
}

#[tokio::test(flavor = "multi_thread")]
async fn clipboard_from_an_unpaired_device_is_rejected_even_with_allow_untrusted() {
    let (sender, receiver) = (Device::new("Visitor"), Device::new("Desk"));
    sender.trust(&receiver).await;
    let running = Running::start(&receiver, Policy { allow_untrusted: true, ..Policy::default() }).await;

    let (sent, handled) = share_clipboard(&sender, &running, "copied").await;
    assert_eq!(sent.unwrap_err().code(), ErrorCode::UntrustedDevice);
    assert_eq!(handled.unwrap_err().code(), ErrorCode::UntrustedDevice);
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_accept_skips_the_question() {
    let (sender, receiver) = (Device::new("Phone"), Device::new("Desk"));
    sender.trust(&receiver).await;
    receiver.trust(&sender).await;
    receiver.trust.set_auto_accept(sender.info.id, true).await.unwrap();
    let running = Running::start(&receiver, Policy::default()).await;

    // This user would decline, but is not asked.
    let user = Receiver::new(false);
    let handled = running.receive_one(&user);
    send_small_file(&sender, running.addr, TransportPreference::Quic).await.unwrap();
    assert!(matches!(handled.await.unwrap().unwrap(), ReceiveOutcome::Saved(_)));
    assert!(!user.asked.load(Ordering::SeqCst));
}

async fn copied_identity_is_rejected_by_the_receiver(preference: TransportPreference) {
    let (original, receiver) = (Device::new("Laptop"), Device::new("Desk"));
    receiver.trust(&original).await;
    let impostor = Device::impostor_of(&original);
    impostor.trust(&receiver).await;
    let running = Running::start(&receiver, Policy::default()).await;

    let user = Receiver::new(true);
    let handled = running.receive_one(&user);
    let error = send_small_file(&impostor, running.addr, preference).await.unwrap_err();
    assert_eq!(error.code(), ErrorCode::FingerprintMismatch);
    assert_eq!(handled.await.unwrap().unwrap_err().code(), ErrorCode::FingerprintMismatch);
}
over_both_transports!(copied_identity_is_rejected_by_the_receiver: impostor_sender_over_tcp, impostor_sender_over_quic);

async fn copied_identity_is_rejected_by_the_sender(preference: TransportPreference) {
    // Discovery advertised the real receiver's fingerprint, but an impostor with the same name answers.
    let (sender, real) = (Device::new("Laptop"), Device::new("Desk"));
    let impostor = Device::impostor_of(&real);
    sender.trust(&real).await;
    sender.trust(&impostor).await;
    let running = Running::start(&impostor, Policy::default()).await;

    let mut options = sender.connect_options(preference, sender.trusted_check());
    options.expected = Some(real.certificate.fingerprint());
    let error = transport::connect(running.addr, &options).await.unwrap_err();
    assert_eq!(error.code(), ErrorCode::FingerprintMismatch);
    // The check also holds in pairing mode, so a lookalike cannot even ask for a PIN.
    options.check = PeerCheck::Pairing;
    let error = transport::connect(running.addr, &options).await.unwrap_err();
    assert_eq!(error.code(), ErrorCode::FingerprintMismatch);
}
over_both_transports!(copied_identity_is_rejected_by_the_sender: impostor_receiver_over_tcp, impostor_receiver_over_quic);

#[tokio::test(flavor = "multi_thread")]
async fn removing_a_trusted_device_forces_pairing_again() {
    let (sender, receiver) = (Device::new("Laptop"), Device::new("Desk"));
    sender.trust(&receiver).await;
    receiver.trust(&sender).await;
    let running = Running::start(&receiver, Policy::default()).await;
    let preference = TransportPreference::Quic;

    let handled = running.receive_one(&Receiver::new(true));
    send_small_file(&sender, running.addr, preference).await.unwrap();
    handled.await.unwrap().unwrap();

    // Removed by another process, like `lanshare trusted remove` while `lanshare receive` runs.
    let other_process = TrustStore::open_at(&receiver.dir.path().join(TRUST_FILE_NAME)).unwrap();
    assert!(other_process.remove(sender.info.id).await.unwrap());

    let handled = running.receive_one(&Receiver::new(true));
    let error = send_small_file(&sender, running.addr, preference).await.unwrap_err();
    assert_eq!(error.code(), ErrorCode::UntrustedDevice);
    assert_eq!(handled.await.unwrap().unwrap_err().code(), ErrorCode::UntrustedDevice);

    let (paired, outcome) = pair_once(&sender, &running, preference, true).await;
    paired.unwrap();
    outcome.unwrap();
    let handled = running.receive_one(&Receiver::new(true));
    send_small_file(&sender, running.addr, preference).await.unwrap();
    handled.await.unwrap().unwrap();
}

// ---------------------------------------------------------------------------------------------------------------
// Hardening.

async fn extra_connections_are_told_the_receiver_is_busy(preference: TransportPreference) {
    let (sender, receiver) = (Device::new("Flood"), Device::new("Desk"));
    sender.trust(&receiver).await;
    // Nobody calls `accept`, so every connection keeps its place.
    let running = Running::start(&receiver, Policy::default()).await;
    let options = sender.connect_options(preference, sender.trusted_check());

    let mut held: Vec<AnyConnection> = Vec::new();
    for _ in 0..MAX_INCOMING_CONNECTIONS {
        let connection = transport::connect(running.addr, &options).await.unwrap();
        // A QUIC stream reaches the receiver only with its first bytes.
        let (mut send, _recv) = connection.open_control().await.unwrap();
        write_message(&mut send, &hello(&sender)).await.unwrap();
        held.push(connection);
    }
    // Let the receiver finish sorting the connections above before the extra one arrives.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let extra = transport::connect(running.addr, &options).await.unwrap();
    let (mut send, mut recv) = extra.open_control().await.unwrap();
    write_message(&mut send, &hello(&sender)).await.unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(5), read_message(&mut recv)).await.unwrap().unwrap();
    assert_eq!(reply, Message::Reject { code: ErrorCode::Busy });
    drop(held);
}
over_both_transports!(extra_connections_are_told_the_receiver_is_busy: busy_over_tcp, busy_over_quic);

fn hello(device: &Device) -> Message {
    Message::Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: device.info.id,
        device_name: device.info.name.clone(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tcp_traffic_shows_no_file_name_or_content() {
    let (sender, receiver) = (Device::new("Laptop"), Device::new("Desk"));
    sender.trust(&receiver).await;
    receiver.trust(&sender).await;
    let running = Running::start(&receiver, Policy::default()).await;
    let (proxy_addr, captured) = start_recording_proxy(running.addr).await;

    let name = "quarterly-secret-report.txt";
    let content = "TOP SECRET CONTENT ".repeat(10_000);
    let source = sender.dir.path().join(name);
    std::fs::write(&source, &content).unwrap();
    let handled = running.receive_one(&Receiver::new(true));
    let options = SendOptions::new(sender.dir.path().join("hash-cache"));
    let connect_options = sender.connect_options(TransportPreference::Tcp, sender.trusted_check());
    let connect = || transport::connect(proxy_addr, &connect_options);
    let (progress, _watcher) = progress_channel();
    send_file(connect, &source, &sender.info, &options, &TransferControl::default(), &progress).await.unwrap();
    let ReceiveOutcome::Saved(saved) = handled.await.unwrap().unwrap() else { panic!("expected the file") };
    assert_eq!(std::fs::read_to_string(saved.path).unwrap(), content);

    let captured = captured.lock().unwrap().clone();
    assert!(captured.len() > content.len(), "the proxy saw the whole transfer");
    for secret in [name.as_bytes(), b"TOP SECRET", b"Laptop", b"Desk"] {
        assert!(!captured.windows(secret.len()).any(|window| window == secret), "plaintext {secret:?} on the wire");
    }
}

/// A TCP proxy in front of `target` that records every byte going through it, in both directions.
async fn start_recording_proxy(target: SocketAddr) -> (SocketAddr, Arc<Mutex<Vec<u8>>>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&captured);
    tokio::spawn(async move {
        while let Ok((client, _)) = listener.accept().await {
            let server = TcpStream::connect(target).await.unwrap();
            let (client_read, client_write) = client.into_split();
            let (server_read, server_write) = server.into_split();
            tokio::spawn(copy_recording(client_read, server_write, Arc::clone(&log)));
            tokio::spawn(copy_recording(server_read, client_write, Arc::clone(&log)));
        }
    });
    (addr, captured)
}

async fn copy_recording(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    log: Arc<Mutex<Vec<u8>>>,
) {
    let mut buffer = vec![0; 64 * 1024];
    while let Ok(read) = from.read(&mut buffer).await {
        if read == 0 || to.write_all(&buffer[..read]).await.is_err() {
            break;
        }
        log.lock().unwrap().extend_from_slice(&buffer[..read]);
    }
    let _ = to.shutdown().await;
}

#[test]
fn fingerprints_differ_per_device() {
    let first = Device::new("A").certificate.fingerprint();
    let second = Device::new("B").certificate.fingerprint();
    assert_ne!(first, second);
    assert_ne!(first, Fingerprint::from_bytes([0; 32]));
}
