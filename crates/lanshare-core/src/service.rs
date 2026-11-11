//! [`LanShareService`]: runs discovery and the receiver together and owns every active transfer. The desktop app and
//! `lanshare receive` both use it, so they behave the same.
//!
//! Everything that happens is reported as one stream of [`ServiceEvent`]s. Questions for the user (accept an offer,
//! type a PIN) are events too; the answers come back through methods such as
//! [`LanShareService::respond_to_offer`] and [`LanShareService::submit_pin`].

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, PoisonError,
    },
    time::Duration,
};

use tokio::{
    sync::{mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
};
use tracing::{debug, info, warn};

use crate::{
    config::Config,
    discovery::{Discovery, DiscoveryOptions, Peer, PeerEvent},
    error::{Error, ErrorCode, Result},
    identity::{DeviceCertificate, DeviceId, DeviceInfo, TrustStore, TrustedDevice},
    pairing::{self, PairingLockout, PairingPeer, PinPrompt, PIN_ENTRY_TIMEOUT},
    paths,
    transfer::{
        check_text, cleanup_stale_parts, list_entries, progress_channel, receive_file, send_clipboard, send_files,
        send_text, IncomingOffer, OfferDecider, Progress, ReceiveOutcome, ReceivePolicy, ReceivedText, SendOptions,
        TransferControl,
    },
    transport::{self, AnyConnection, ConnectDiagnosis, ConnectOptions, Listener, PeerCheck, TransportPreference},
};

/// How long an offer waits for the user before it is declined.
pub const OFFER_TIMEOUT: Duration = Duration::from_mins(5);

/// Progress events for one transfer are sent at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

/// Identifies a transfer within one running service.
pub type TransferId = u64;

/// Identifies a question for the user (an offer or a pairing) within one running service.
pub type RequestId = u64;

/// Which way a transfer goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// From this device.
    Send,
    /// To this device.
    Receive,
}

/// What a transfer is about, sent once when it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferInfo {
    /// Its id in this service.
    pub id: TransferId,
    /// Which way it goes.
    pub direction: Direction,
    /// The other device's id.
    pub peer_id: DeviceId,
    /// The other device's name.
    pub peer_name: String,
    /// What to call it: the file or folder name (the first one, if several were sent).
    pub file_name: String,
    /// Number of files, not counting folders.
    pub file_count: usize,
    /// Every entry's `/`-separated path, in offer order. [`Progress::current_entry`] indexes into it.
    pub entry_names: Vec<String>,
    /// Total size in bytes.
    pub size: u64,
}

/// Everything that happens in the service, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceEvent {
    /// A device appeared, changed or left.
    Peer(PeerEvent),
    /// A device offers a file. Answer with [`LanShareService::respond_to_offer`] within [`OFFER_TIMEOUT`].
    IncomingOffer {
        /// Pass this to `respond_to_offer`.
        request_id: RequestId,
        /// What is offered.
        offer: IncomingOffer,
    },
    /// An offer is no longer waiting for an answer (answered, timed out, or the sender left).
    OfferClosed {
        /// The offer's request id.
        request_id: RequestId,
    },
    /// A transfer started moving data (or, for sending, is waiting for the receiver to accept).
    TransferStarted(TransferInfo),
    /// A transfer moved data or was paused or resumed.
    TransferProgress {
        /// The transfer.
        id: TransferId,
        /// Bytes so far.
        progress: Progress,
        /// Whether either device paused it.
        paused: bool,
    },
    /// A transfer finished.
    TransferComplete {
        /// The transfer.
        id: TransferId,
        /// Where a received file was saved; `None` for sent files.
        path: Option<PathBuf>,
    },
    /// A transfer failed or was cancelled.
    TransferFailed {
        /// The transfer.
        id: TransferId,
        /// Why.
        error: ErrorCode,
        /// Why the other device could not be reached, if that is why it failed.
        diagnosis: Option<ConnectDiagnosis>,
    },
    /// An offer was refused before it started because of a problem only the user of this device can fix, such as a
    /// download folder that is full or read-only.
    ReceiveRefused {
        /// Why.
        error: ErrorCode,
    },
    /// A paired device sent a text. Show it; it is not stored anywhere.
    TextReceived(ReceivedText),
    /// A paired device this one syncs the clipboard with copied a text: put it on the clipboard. It is not stored
    /// anywhere else.
    ClipboardReceived(ReceivedText),
    /// Another device asks to pair: show `pin` to the user.
    PairingPin {
        /// The pairing.
        request_id: RequestId,
        /// The device asking.
        peer: PairingPeer,
        /// The PIN to show.
        pin: String,
    },
    /// A pairing ended, started from either device.
    PairingResult {
        /// The pairing.
        request_id: RequestId,
        /// The newly trusted device, or why pairing failed.
        result: std::result::Result<TrustedDevice, ErrorCode>,
        /// Why the other device could not be reached, if that is why pairing failed.
        diagnosis: Option<ConnectDiagnosis>,
    },
}

/// What the service needs to start.
#[derive(Debug, Clone)]
pub struct ServiceOptions {
    /// Settings from `config.toml`.
    pub config: Config,
    /// This device's certificate.
    pub certificate: DeviceCertificate,
    /// Paired devices.
    pub trust: TrustStore,
    /// Use mDNS for discovery (UDP broadcast follows `config.udp_fallback`).
    pub mdns: bool,
    /// Overrides `config.port`.
    pub port: Option<u16>,
    /// Overrides `config.download_dir`.
    pub download_dir: Option<PathBuf>,
    /// Accept clipboard texts from paired devices with `sync_clipboard` on, reported as
    /// [`ServiceEvent::ClipboardReceived`]. Only a frontend that can write the clipboard turns this on.
    pub clipboard: bool,
}

/// Discovery, the receiver and all transfers of this device.
///
/// Cloning gives another handle to the same service.
#[derive(Clone)]
pub struct LanShareService {
    inner: Arc<Inner>,
}

struct Inner {
    local: DeviceInfo,
    certificate: DeviceCertificate,
    trust: TrustStore,
    download_dir: PathBuf,
    quic_enabled: bool,
    settings: Mutex<Config>,
    clipboard: bool,
    lockout: Arc<PairingLockout>,
    events: mpsc::UnboundedSender<ServiceEvent>,
    discovery: Mutex<Option<Discovery>>,
    transfers: Mutex<HashMap<TransferId, TransferControl>>,
    offers: Mutex<HashMap<RequestId, oneshot::Sender<bool>>>,
    pins: Mutex<HashMap<RequestId, oneshot::Sender<Option<String>>>>,
    next_id: AtomicU64,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl LanShareService {
    /// Starts listening (TCP and QUIC), announcing this device, and looking for others.
    ///
    /// Must be called inside a tokio runtime. Old unfinished transfers in the download folder are cleaned up first.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the port cannot be bound, the download folder cannot be created, or discovery cannot start.
    pub async fn start(options: ServiceOptions) -> Result<(Self, mpsc::UnboundedReceiver<ServiceEvent>)> {
        let config = options.config;
        let download_dir = options.download_dir.unwrap_or_else(|| config.download_dir.clone());
        tokio::fs::create_dir_all(&download_dir).await.map_err(Error::Io)?;
        if let Err(error) = cleanup_stale_parts(&download_dir, config.stale_part_age()).await {
            warn!(%error, "could not clean up old unfinished transfers");
        }
        let listener = Listener::bind(options.port.unwrap_or(config.port), &options.certificate).await?;
        let mut local = config.device_info(options.certificate.fingerprint());
        local.port = listener.port();
        let discovery_options = DiscoveryOptions {
            advertise: true,
            mdns: options.mdns,
            udp_fallback: config.udp_fallback,
            ..DiscoveryOptions::default()
        };
        let (discovery, peer_events) = Discovery::start(&local, discovery_options)?;
        let (events, receiver) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            local,
            certificate: options.certificate,
            trust: options.trust,
            download_dir,
            quic_enabled: listener.quic_enabled(),
            settings: Mutex::new(config),
            clipboard: options.clipboard,
            lockout: Arc::new(PairingLockout::new()),
            events,
            discovery: Mutex::new(Some(discovery)),
            transfers: Mutex::default(),
            offers: Mutex::default(),
            pins: Mutex::default(),
            next_id: AtomicU64::new(1),
            tasks: Mutex::default(),
        });
        let accept = tokio::spawn(accept_loop(Arc::clone(&inner), listener));
        let peers = tokio::spawn(forward_peers(Arc::clone(&inner), peer_events));
        lock(&inner.tasks).extend([accept, peers]);
        Ok((Self { inner }, receiver))
    }

    /// This device as it introduces itself, with the port actually in use.
    #[must_use]
    pub fn local(&self) -> &DeviceInfo {
        &self.inner.local
    }

    /// Where received files are saved.
    #[must_use]
    pub fn download_dir(&self) -> &Path {
        &self.inner.download_dir
    }

    /// Whether QUIC connections are accepted (the UDP port could be opened).
    #[must_use]
    pub fn quic_enabled(&self) -> bool {
        self.inner.quic_enabled
    }

    /// Paired devices.
    #[must_use]
    pub fn trust(&self) -> &TrustStore {
        &self.inner.trust
    }

    /// Every device currently found, sorted by name.
    #[must_use]
    pub fn peers(&self) -> Vec<Peer> {
        lock(&self.inner.discovery).as_ref().map(Discovery::peers).unwrap_or_default()
    }

    /// Applies settings that take effect without a restart: streams and chunk size (for new transfers), bandwidth limit
    /// (also for running transfers)
    /// and `allow_untrusted`. Device name, port and download folder need a new service.
    pub fn update_settings(&self, config: Config) {
        let limit = config.bandwidth_limit_mbps;
        *lock(&self.inner.settings) = config;
        for control in lock(&self.inner.transfers).values() {
            control.set_bandwidth_limit(limit);
        }
    }

    /// Sends the files and folders at `paths` to the device `peer_id` as one transfer, and returns its id.
    ///
    /// A device that is not paired makes the transfer fail with [`ErrorCode::UntrustedDevice`]; pair first with
    /// [`LanShareService::start_pairing`].
    ///
    /// # Errors
    ///
    /// [`Error::PeerUnreachable`] if no device with this id is currently found.
    pub fn send(&self, peer_id: DeviceId, paths: Vec<PathBuf>) -> Result<TransferId> {
        let peer = self.find_peer(peer_id)?;
        let id = self.inner.new_id();
        let control = TransferControl::new(lock(&self.inner.settings).bandwidth_limit_mbps);
        lock(&self.inner.transfers).insert(id, control.clone());
        let inner = Arc::clone(&self.inner);
        self.inner.spawn(async move {
            let result = send_all(&inner, &peer, id, &control, &paths).await;
            inner.finish(id, result.map(|()| None));
        });
        Ok(id)
    }

    /// Sends `text` to the device `peer_id` and waits until that device has it. Texts are not transfers: they have
    /// no id, progress or history, and the other device shows them without asking.
    ///
    /// # Errors
    ///
    /// - [`Error::TextTooLong`] or [`Error::ProtocolViolation`] (empty text) before connecting
    /// - [`Error::PeerUnreachable`] if no device with this id is currently found, or none of its addresses answers
    /// - [`Error::UntrustedDevice`] if the two devices are not paired; pair first with
    ///   [`LanShareService::start_pairing`]
    /// - [`Error::VersionMismatch`] if the other device is too old for text
    pub async fn send_text(&self, peer_id: DeviceId, text: &str) -> Result<()> {
        check_text(text)?;
        let peer = self.find_peer(peer_id)?;
        let inner = &self.inner;
        let options = inner.connect_options(&peer, PeerCheck::Trusted(inner.trust.clone()));
        let connection: AnyConnection = transport::connect_any(&peer.socket_addrs(), &options).await?;
        send_text(&connection, &inner.local, text).await
    }

    /// Puts `text`, just copied on this device, on the clipboard of every paired device found right now that has
    /// `sync_clipboard` on, all at once. Returns how many have it. Devices that refuse or cannot be reached are
    /// skipped: syncing is best effort, and the next copy tries again.
    ///
    /// # Errors
    ///
    /// [`Error::TextTooLong`] or [`Error::ProtocolViolation`] (empty text) before connecting to any device.
    pub async fn share_clipboard(&self, text: &str) -> Result<usize> {
        check_text(text)?;
        let targets: Vec<Peer> = self
            .peers()
            .into_iter()
            .filter(|peer| self.inner.trust.get_by_id(peer.id).is_some_and(|device| device.sync_clipboard))
            .collect();
        let text: Arc<str> = Arc::from(text);
        let mut sends = JoinSet::new();
        for peer in targets {
            let (inner, text) = (Arc::clone(&self.inner), Arc::clone(&text));
            sends.spawn(async move {
                let options = inner.connect_options(&peer, PeerCheck::Trusted(inner.trust.clone()));
                let result = match transport::connect_any(&peer.socket_addrs(), &options).await {
                    Ok(connection) => send_clipboard(&connection, &inner.local, &text).await,
                    Err(error) => Err(error),
                };
                if let Err(error) = &result {
                    debug!(device = %peer.name, %error, "clipboard not shared with this device");
                }
                result.is_ok()
            });
        }
        let mut delivered = 0;
        while let Some(result) = sends.join_next().await {
            delivered += usize::from(result.unwrap_or(false));
        }
        Ok(delivered)
    }

    /// Answers an [`ServiceEvent::IncomingOffer`]. Returns `false` if the offer is no longer waiting.
    #[must_use = "`false` means the answer came too late"]
    pub fn respond_to_offer(&self, request_id: RequestId, accept: bool) -> bool {
        lock(&self.inner.offers).remove(&request_id).is_some_and(|answer| answer.send(accept).is_ok())
    }

    /// Pauses a transfer; the other device is told. Returns `false` for an unknown id.
    pub fn pause(&self, id: TransferId) -> bool {
        self.with_transfer(id, TransferControl::pause)
    }

    /// Resumes a paused transfer. Returns `false` for an unknown id.
    pub fn resume(&self, id: TransferId) -> bool {
        self.with_transfer(id, TransferControl::resume)
    }

    /// Cancels a transfer, running or queued; the other device is told. Returns `false` for an unknown id.
    pub fn cancel(&self, id: TransferId) -> bool {
        self.with_transfer(id, TransferControl::cancel)
    }

    /// Pauses every active transfer.
    pub fn pause_all(&self) {
        for control in lock(&self.inner.transfers).values() {
            control.pause();
        }
    }

    /// Starts pairing with the device `peer_id`. The other device shows a PIN; pass what the user types to
    /// [`LanShareService::submit_pin`] with the returned id. The end is reported as [`ServiceEvent::PairingResult`].
    ///
    /// # Errors
    ///
    /// [`Error::PeerUnreachable`] if no device with this id is currently found.
    pub fn start_pairing(&self, peer_id: DeviceId) -> Result<RequestId> {
        let peer = self.find_peer(peer_id)?;
        let request_id = self.inner.new_id();
        let inner = Arc::clone(&self.inner);
        self.inner.spawn(async move {
            let result = pair_with(&inner, &peer, request_id).await;
            lock(&inner.pins).remove(&request_id);
            if let Err(error) = &result {
                info!(%error, "pairing failed");
            }
            let diagnosis = result.as_ref().err().and_then(Error::connect_diagnosis);
            let result = result.map_err(|error| error.code());
            inner.emit(ServiceEvent::PairingResult { request_id, result, diagnosis });
        });
        Ok(request_id)
    }

    /// Gives the PIN the user typed for a pairing started with [`LanShareService::start_pairing`], or `None` to
    /// cancel it. Returns `false` if that pairing is not waiting for a PIN (yet, or any more).
    #[must_use = "`false` means the pairing is not waiting for a PIN"]
    pub fn submit_pin(&self, request_id: RequestId, pin: Option<String>) -> bool {
        lock(&self.inner.pins).remove(&request_id).is_some_and(|answer| answer.send(pin).is_ok())
    }

    /// Stops discovery (saying goodbye to other devices), the receiver and every transfer.
    pub async fn shutdown(&self) {
        for control in lock(&self.inner.transfers).values() {
            control.cancel();
        }
        let discovery = lock(&self.inner.discovery).take();
        if let Some(discovery) = discovery {
            discovery.shutdown().await;
        }
        for task in lock(&self.inner.tasks).drain(..) {
            task.abort();
        }
    }

    fn find_peer(&self, peer_id: DeviceId) -> Result<Peer> {
        self.peers().into_iter().find(|peer| peer.id == peer_id).ok_or_else(Error::peer_not_found)
    }

    fn with_transfer(&self, id: TransferId, action: impl FnOnce(&TransferControl)) -> bool {
        lock(&self.inner.transfers).get(&id).map(action).is_some()
    }
}

impl Inner {
    fn new_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn emit(&self, event: ServiceEvent) {
        if self.events.send(event).is_err() {
            debug!("nobody is listening for service events");
        }
    }

    fn spawn(&self, task: impl std::future::Future<Output = ()> + Send + 'static) {
        let mut tasks = lock(&self.tasks);
        tasks.retain(|task| !task.is_finished());
        tasks.push(tokio::spawn(task));
    }

    /// Forgets a transfer and reports how it ended.
    fn finish(&self, id: TransferId, result: Result<Option<PathBuf>>) {
        lock(&self.transfers).remove(&id);
        match result {
            Ok(path) => self.emit(ServiceEvent::TransferComplete { id, path }),
            Err(error) => {
                info!(id, %error, "transfer failed");
                let diagnosis = error.connect_diagnosis();
                self.emit(ServiceEvent::TransferFailed { id, error: error.code(), diagnosis });
            }
        }
    }

    fn connect_options(&self, peer: &Peer, check: PeerCheck) -> ConnectOptions {
        ConnectOptions {
            preference: TransportPreference::Auto,
            certificate: self.certificate.clone(),
            check,
            expected: Some(peer.fingerprint),
            peer_os: Some(peer.os),
        }
    }

    /// Publishes progress and pause changes of one transfer, at most every [`PROGRESS_INTERVAL`], until the
    /// transfer drops its progress sender.
    fn forward_progress(
        self: &Arc<Self>,
        id: TransferId,
        control: &TransferControl,
        mut progress: watch::Receiver<Progress>,
    ) {
        let inner = Arc::clone(self);
        let mut paused = control.subscribe();
        self.spawn(async move {
            loop {
                tokio::select! {
                    changed = progress.changed() => if changed.is_err() { break },
                    changed = paused.changed() => if changed.is_err() { break },
                }
                let (now, is_paused) = (*progress.borrow_and_update(), *paused.borrow_and_update());
                inner.emit(ServiceEvent::TransferProgress { id, progress: now, paused: is_paused });
                tokio::time::sleep(PROGRESS_INTERVAL).await;
            }
        });
    }
}

/// Sends `paths` to `peer` as transfer `id`.
async fn send_all(
    inner: &Arc<Inner>,
    peer: &Peer,
    id: TransferId,
    control: &TransferControl,
    paths: &[PathBuf],
) -> Result<()> {
    let entries = list_entries(paths).await?;
    let file_name = entries.first().map(|(name, _)| name.split('/').next().unwrap_or_default().to_owned());
    inner.emit(ServiceEvent::TransferStarted(TransferInfo {
        id,
        direction: Direction::Send,
        peer_id: peer.id,
        peer_name: peer.name.clone(),
        file_name: file_name.unwrap_or_default(),
        file_count: entries.iter().filter(|(name, _)| !name.ends_with('/')).count(),
        size: entries.iter().map(|(_, size)| size).sum(),
        entry_names: entries.into_iter().map(|(name, _)| name).collect(),
    }));
    let (streams, chunk_size) = {
        let settings = lock(&inner.settings);
        (settings.streams, settings.chunk_size())
    };
    let hash_cache = tokio::task::spawn_blocking(paths::hash_cache_dir)
        .await
        .map_err(|error| Error::Io(std::io::Error::other(error)))??;
    let options = SendOptions { chunk_size, streams, ..SendOptions::new(hash_cache) };
    let connect_options = inner.connect_options(peer, PeerCheck::Trusted(inner.trust.clone()));
    let addrs: Vec<SocketAddr> = peer.socket_addrs();
    let connect = || transport::connect_any(&addrs, &connect_options);
    let (progress, watcher) = progress_channel();
    inner.forward_progress(id, control, watcher);
    send_files(connect, paths, &inner.local, &options, control, &progress).await
}

/// Connects to `peer` in pairing mode and runs the PIN exchange, asking for the PIN through
/// [`LanShareService::submit_pin`].
async fn pair_with(inner: &Arc<Inner>, peer: &Peer, request_id: RequestId) -> Result<TrustedDevice> {
    let connection: AnyConnection =
        transport::connect_any(&peer.socket_addrs(), &inner.connect_options(peer, PeerCheck::Pairing)).await?;
    let prompt = ServicePin { inner: Arc::clone(inner), request_id };
    pairing::pair(&connection, &inner.local, &inner.trust, &prompt).await
}

/// Waits for [`LanShareService::submit_pin`].
struct ServicePin {
    inner: Arc<Inner>,
    request_id: RequestId,
}

impl PinPrompt for ServicePin {
    async fn ask_pin(&self, _receiver: &PairingPeer) -> Option<String> {
        let (answer, waiting) = oneshot::channel();
        lock(&self.inner.pins).insert(self.request_id, answer);
        tokio::time::timeout(PIN_ENTRY_TIMEOUT, waiting).await.ok()?.ok()?
    }
}

/// Hands each incoming connection to its own task.
async fn accept_loop(inner: Arc<Inner>, listener: Listener) {
    loop {
        match listener.accept().await {
            Ok(connection) => {
                let inner_for_task = Arc::clone(&inner);
                inner.spawn(handle_connection(inner_for_task, connection));
            }
            Err(error) => {
                warn!(%error, "the receiver stopped accepting connections");
                return;
            }
        }
    }
}

async fn forward_peers(inner: Arc<Inner>, mut events: mpsc::UnboundedReceiver<PeerEvent>) {
    while let Some(event) = events.recv().await {
        inner.emit(ServiceEvent::Peer(event));
    }
}

/// Receives on one connection: an offer, a text, a pairing request, or a benchmark.
async fn handle_connection(inner: Arc<Inner>, connection: AnyConnection) {
    let id = inner.new_id();
    let control = TransferControl::default();
    lock(&inner.transfers).insert(id, control.clone());
    let decider =
        ConnectionDecider { inner: Arc::clone(&inner), id, started: Mutex::new(false), pairing: Mutex::new(None) };
    let policy = ReceivePolicy {
        local: inner.local.clone(),
        trust: inner.trust.clone(),
        allow_untrusted: lock(&inner.settings).allow_untrusted,
        accept_clipboard: inner.clipboard,
        lockout: Arc::clone(&inner.lockout),
    };
    let (progress, watcher) = progress_channel();
    inner.forward_progress(id, &control, watcher);
    let result = receive_file(&connection, &inner.download_dir, &decider, &policy, &control, &progress).await;
    drop(progress);

    let pairing = *lock(&decider.pairing);
    let started = *lock(&decider.started);
    match result {
        Ok(ReceiveOutcome::Saved(saved)) => inner.finish(id, Ok(Some(saved.path))),
        Ok(ReceiveOutcome::Paired(device)) => {
            lock(&inner.transfers).remove(&id);
            if let Some(request_id) = pairing {
                inner.emit(ServiceEvent::PairingResult { request_id, result: Ok(device), diagnosis: None });
            }
        }
        Ok(ReceiveOutcome::Text(text)) => {
            lock(&inner.transfers).remove(&id);
            inner.emit(ServiceEvent::TextReceived(text));
        }
        Ok(ReceiveOutcome::Clipboard(text)) => {
            lock(&inner.transfers).remove(&id);
            inner.emit(ServiceEvent::ClipboardReceived(text));
        }
        Ok(ReceiveOutcome::Declined(_) | ReceiveOutcome::Benchmark { .. }) => {
            lock(&inner.transfers).remove(&id);
        }
        Err(error) if started => inner.finish(id, Err(error)),
        Err(error) => {
            lock(&inner.transfers).remove(&id);
            if let Some(request_id) = pairing {
                inner.emit(ServiceEvent::PairingResult { request_id, result: Err(error.code()), diagnosis: None });
            } else if matches!(error, Error::PermissionDenied { .. } | Error::InsufficientSpace { .. }) {
                warn!(%error, "refused an incoming offer");
                inner.emit(ServiceEvent::ReceiveRefused { error: error.code() });
            } else {
                info!(%error, "incoming connection ended before a transfer started");
            }
        }
    }
}

/// Answers the receiver's questions for one connection through events.
struct ConnectionDecider {
    inner: Arc<Inner>,
    id: TransferId,
    started: Mutex<bool>,
    /// The request id of a pairing on this connection, once its PIN was shown.
    pairing: Mutex<Option<RequestId>>,
}

impl OfferDecider for ConnectionDecider {
    async fn decide(&self, offer: &IncomingOffer) -> bool {
        let request_id = self.inner.new_id();
        let (answer, waiting) = oneshot::channel();
        lock(&self.inner.offers).insert(request_id, answer);
        self.inner.emit(ServiceEvent::IncomingOffer { request_id, offer: offer.clone() });
        let accepted = matches!(tokio::time::timeout(OFFER_TIMEOUT, waiting).await, Ok(Ok(true)));
        lock(&self.inner.offers).remove(&request_id);
        self.inner.emit(ServiceEvent::OfferClosed { request_id });
        accepted
    }

    fn accepted(&self, offer: &IncomingOffer) {
        *lock(&self.started) = true;
        self.inner.emit(ServiceEvent::TransferStarted(TransferInfo {
            id: self.id,
            direction: Direction::Receive,
            peer_id: offer.device_id,
            peer_name: offer.device_name.clone(),
            file_name: offer.file_name.clone(),
            file_count: offer.file_count,
            entry_names: offer.entry_names.clone(),
            size: offer.size,
        }));
    }

    fn show_pin(&self, peer: &PairingPeer, pin: &str) {
        let request_id = self.inner.new_id();
        *lock(&self.pairing) = Some(request_id);
        self.inner.emit(ServiceEvent::PairingPin { request_id, peer: peer.clone(), pin: pin.to_owned() });
    }
}

/// Locks a mutex, carrying on if another thread panicked while holding it: every update here is a single
/// insert, remove or assignment, so the data is never half-changed.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
