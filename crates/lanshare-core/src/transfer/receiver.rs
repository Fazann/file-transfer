//! Receiving side: checks the hello, applies the trust policy (or answers a pairing request), checks the offer,
//! resumes from a `.lanshare-state` file when the same transfer comes back, verifies every chunk arriving on the data
//! streams, and saves the file once its root hash matches.

use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::{mpsc, watch},
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::{
    bench::receive_bench,
    chunk::{hash_chunk, ChunkHash, ChunkLayout},
    control::{Activity, ControlWriter, PauseSync, TransferControl},
    manifest::MAX_ENTRIES,
    path::{sanitize_relative_path, RelativePath},
    peer_error, read_exact_tracked,
    scheduler::MAX_STREAMS,
    state::{part_path, state_path, ResumeState},
    text::{check_text, ReceivedText},
    writer::ChunkWriter,
    Progress, SAVE_EVERY_CHUNKS,
};
use crate::{
    error::{Error, ErrorCode, Result},
    identity::{DeviceId, DeviceInfo, Fingerprint, TrustStore, TrustedDevice},
    pairing::{PairingAnswer, PairingLockout, PairingPeer},
    protocol::{
        is_supported_version, read_message, read_message_or_end, EntryResume, Message, OfferEntry, PROTOCOL_VERSION,
    },
    transport::{Connection, PeerSession, RecvHalf, HANDSHAKE_TIMEOUT},
};

/// How many `name (n).ext` candidates to try before giving up on finding a free file name.
const MAX_NAME_ATTEMPTS: u32 = 10_000;

/// Chunks waiting between the data stream readers and the writer. With one chunk per reader and the one being
/// written, this bounds how many chunk buffers exist at once.
const CHUNK_QUEUE: usize = 4;

/// A chunk that fails its hash check this many times aborts the transfer with [`ErrorCode::HashMismatch`].
const MAX_HASH_ATTEMPTS: u32 = 3;

/// After the final `Done`, how long to wait for the sender to close, so the `Done` is not cut off.
const LINGER: Duration = Duration::from_secs(5);

/// An offer received from a sender, after its paths were checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingOffer {
    /// Stable id of the sending device.
    pub device_id: DeviceId,
    /// Name the sender reported for its device.
    pub device_name: String,
    /// Fingerprint of the sender's certificate, proven by TLS.
    pub fingerprint: Fingerprint,
    /// Whether the sender is a paired device. `false` only reaches the decider when `allow_untrusted` is on.
    pub trusted: bool,
    /// What to call the offer: the name of its file or folder (the first one, if several were sent).
    pub file_name: String,
    /// Number of files, not counting folders.
    pub file_count: usize,
    /// Every entry's `/`-separated path, in offer order; folders end with `/`. Progress refers to them by index.
    pub entry_names: Vec<String>,
    /// Total size in bytes.
    pub size: u64,
}

/// Talks to the receiving user: decides whether to accept an incoming offer, and shows the PIN when another device
/// asks to pair. The CLI uses the terminal; the app will use its UI.
///
/// `decide` is written as "returns a `Future`" instead of `async fn` so we can require the future to be `Send` (safe
/// to move between threads). Implementors can still write a plain `async fn decide(...)`.
pub trait OfferDecider: Send + Sync {
    /// Returns `true` to accept the offer. Not called for paired devices with `auto_accept` on.
    fn decide(&self, offer: &IncomingOffer) -> impl Future<Output = bool> + Send;

    /// Called once an offer is accepted, however it was accepted (asked, auto-accepted, or a resumed transfer),
    /// right before the data starts. Does nothing unless overridden.
    fn accepted(&self, _offer: &IncomingOffer) {}

    /// Shows `pin` to the user, who reads it out to the person at `peer`. Must not block.
    fn show_pin(&self, peer: &PairingPeer, pin: &str);
}

/// Who may send to this device, and what pairing needs.
#[derive(Debug, Clone)]
pub struct ReceivePolicy {
    /// This device, as it introduces itself when pairing.
    pub local: DeviceInfo,
    /// Paired devices.
    pub trust: TrustStore,
    /// Let unpaired devices offer files (`allow_untrusted` in `config.toml`).
    pub allow_untrusted: bool,
    /// Failed pairing attempts, shared by every connection.
    pub lockout: Arc<PairingLockout>,
}

/// A transfer that was received and saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedFile {
    /// Where it was saved: the file or folder for a single item (a file may get `name (1).ext` if its name was
    /// taken), or the download folder when several items were sent.
    pub path: PathBuf,
    /// The offer that was accepted.
    pub offer: IncomingOffer,
}

/// How a successful receive ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiveOutcome {
    /// The file was saved.
    Saved(ReceivedFile),
    /// The decider declined the offer and the sender was told so.
    Declined(IncomingOffer),
    /// The sender ran a benchmark (`lanshare bench`); the data was discarded.
    Benchmark {
        /// Name the sender reported for its device.
        device_name: String,
        /// Bytes received.
        bytes: u64,
    },
    /// The sender asked to pair, the PIN matched, and it is now trusted.
    Paired(TrustedDevice),
    /// A paired device sent a text. Nothing was written to disk.
    Text(ReceivedText),
}

/// Handles one incoming connection: a transfer saved into `download_dir`, a text, a benchmark, or a pairing request.
///
/// Senders are checked against `policy` right after their `Hello`: paired devices may offer files (accepted without
/// asking if their `auto_accept` is on); unpaired ones are rejected with [`Error::UntrustedDevice`] unless
/// `allow_untrusted` is on. Texts are taken from paired devices only, whatever `allow_untrusted` says. A device whose
/// id is paired with a different certificate gets [`Error::FingerprintMismatch`]. Any device may ask to pair.
///
/// Data is written to `<name>.lanshare-part` with resume state in `<name>.lanshare-state`. If the transfer stops
/// (error, dropped connection, or this future is dropped), both files are kept, and when the sender offers the same
/// transfer again it continues from the chunks already received, without asking `decider` again. The finished file
/// gets the offered name, or `name (1).ext` and so on if that is taken. `control` pauses and resumes the transfer
/// from this side.
///
/// # Errors
///
/// - [`Error::VersionMismatch`], [`Error::UntrustedDevice`], [`Error::FingerprintMismatch`],
///   [`Error::InvalidFileName`], [`Error::InsufficientSpace`], [`Error::PermissionDenied`] (the sender is told with
///   `Reject`; a write refused later is reported with `Error` instead)
/// - [`Error::PairingFailed`], [`Error::PairingLocked`] for a failed pairing request
/// - [`Error::HashMismatch`] if a chunk fails its hash three times or the file fails its root hash
/// - [`Error::ProtocolViolation`] if messages arrive out of order or do not fit the offer
/// - [`Error::ConnectionLost`] if the sender disconnects or goes quiet for [`IDLE_TIMEOUT`](super::IDLE_TIMEOUT)
/// - [`Error::Io`] if the file cannot be created or written
pub async fn receive_file<C, D>(
    connection: &C,
    download_dir: &Path,
    decider: &D,
    policy: &ReceivePolicy,
    control: &TransferControl,
    progress: &watch::Sender<Progress>,
) -> Result<ReceiveOutcome>
where
    C: Connection,
    D: OfferDecider,
{
    let (send, mut recv) = connection.open_control().await?;
    let writer = ControlWriter::new(send);
    let request = Incoming { connection, writer: &writer, decider, policy };
    let result = tokio::select! {
        result = request.handle(&mut recv, download_dir, control, progress) => result,
        () = control.cancelled() => {
            info!("transfer cancelled on this device");
            // Best effort: tell the sender, so it stops instead of reconnecting.
            let _ = writer.send(&Message::Error { code: ErrorCode::Cancelled, detail: None }).await;
            Err(Error::Cancelled)
        }
    };
    // Whatever the outcome, the last message (`Done`, `Reject` or `Error`) must reach the sender before the
    // connection is dropped: dropping a QUIC connection discards data that is still on its way.
    linger(&writer, &mut recv).await;
    result
}

/// One incoming connection, with what is needed to answer it.
struct Incoming<'a, C, D> {
    connection: &'a C,
    writer: &'a ControlWriter,
    decider: &'a D,
    policy: &'a ReceivePolicy,
}

impl<C: Connection, D: OfferDecider> Incoming<'_, C, D> {
    /// Everything after the control channel is open: hello, trust check or pairing, offer, and the transfer itself.
    async fn handle<R: AsyncRead + Unpin>(
        &self,
        recv: &mut R,
        download_dir: &Path,
        control: &TransferControl,
        progress: &watch::Sender<Progress>,
    ) -> Result<ReceiveOutcome> {
        let writer = self.writer;
        let (device_id, device_name) = read_hello(recv, writer).await?;
        let session = self.connection.session();
        let request = read_handshake(recv).await?;
        if matches!(request, Message::PairRequest) {
            let sender = PairingPeer { device_id, device_name, fingerprint: session.fingerprint };
            return self.answer_pairing(recv, sender).await.map(ReceiveOutcome::Paired);
        }
        let trusted = match check_sender(self.policy, session, device_id).await {
            Ok(trusted) => trusted,
            Err(error) => return reject(writer, error).await,
        };
        if let Message::TextOffer { text } = request {
            // A text is shown without asking, so `allow_untrusted` does not cover it.
            if trusted.is_none() {
                info!(fingerprint = %session.fingerprint, "rejecting a text from an unpaired device");
                return reject(writer, Error::UntrustedDevice).await;
            }
            if let Err(error) = check_text(&text) {
                return reject(writer, error).await;
            }
            writer.send(&Message::Done).await?;
            info!(device = %device_name, bytes = text.len(), "text received");
            let fingerprint = session.fingerprint;
            return Ok(ReceiveOutcome::Text(ReceivedText { device_id, device_name, fingerprint, text }));
        }
        let offer = match check_offer(request, writer).await? {
            Request::Files(offer) => offer,
            Request::Bench { transfer_id, size } => {
                let bytes = receive_bench(self.connection, recv, writer, transfer_id, size).await?;
                return Ok(ReceiveOutcome::Benchmark { device_name, bytes });
            }
        };
        let incoming = IncomingOffer {
            device_id,
            device_name,
            fingerprint: session.fingerprint,
            trusted: trusted.is_some(),
            file_name: offer.summary_name(),
            file_count: offer.file_count(),
            entry_names: offer.entries.iter().map(|entry| entry.path.as_wire()).collect(),
            size: offer.size(),
        };
        let auto_accept = trusted.is_some_and(|device| device.auto_accept);
        self.receive_offer(recv, download_dir, offer, incoming, auto_accept, control, progress).await
    }

    async fn answer_pairing<R: AsyncRead + Unpin>(&self, recv: &mut R, sender: PairingPeer) -> Result<TrustedDevice> {
        info!(device = %sender.device_name, fingerprint = %sender.fingerprint, "pairing request");
        let policy = self.policy;
        let answer = PairingAnswer { local: &policy.local, trust: &policy.trust, lockout: &policy.lockout, sender };
        let decider = self.decider;
        answer.run(recv, self.writer, self.connection.session(), |peer, pin| decider.show_pin(peer, pin)).await
    }

    /// Asks the decider about a new offer (unless `auto_accept`), then receives and saves the file.
    #[allow(clippy::too_many_arguments)]
    async fn receive_offer<R: AsyncRead + Unpin>(
        &self,
        recv: &mut R,
        download_dir: &Path,
        offer: ValidOffer,
        incoming: IncomingOffer,
        auto_accept: bool,
        control: &TransferControl,
        progress: &watch::Sender<Progress>,
    ) -> Result<ReceiveOutcome> {
        let writer = self.writer;
        let targets = match prepare_targets(download_dir, &offer).await {
            Ok(targets) => targets,
            Err(error) => return reject(writer, error).await,
        };
        // A transfer we are resuming was accepted before; only new transfers ask.
        let is_new = targets.iter().flatten().all(|target| target.resume.is_none());
        if is_new && !auto_accept && !self.decider.decide(&incoming).await {
            info!(name = %incoming.file_name, "offer declined");
            writer.send(&Message::Reject { code: ErrorCode::Declined }).await?;
            return Ok(ReceiveOutcome::Declined(incoming));
        }
        // From here on, each file's part saves its resume state if anything fails or this future is dropped.
        let mut parts = Parts::new(download_dir, &offer, targets);

        self.decider.accepted(&incoming);
        writer.send(&Message::Accept { resume: parts.resume_list() }).await?;
        let resumed = parts.received_bytes();
        let total = offer.size();
        info!(entries = offer.entries.len(), total, resumed, "receiving");
        progress.send_replace(Progress { transferred: resumed, total, resumed, current_entry: 0 });

        let transfer_id = offer.transfer_id;
        let session = DataSession { connection: self.connection, transfer_id, writer, control, progress };
        match session.receive_all(recv, &mut parts).await {
            Ok(()) => {
                parts.remove_states().await;
                writer.send(&Message::Done).await?;
                let path = parts.saved_path();
                info!(path = %path.display(), total, "received");
                Ok(ReceiveOutcome::Saved(ReceivedFile { path, offer: incoming }))
            }
            Err(error) => {
                if !matches!(error, Error::ConnectionLost) {
                    // Best effort: the sender may already be gone.
                    let message = Message::Error { code: error.code(), detail: None };
                    if let Err(send_error) = writer.send(&message).await {
                        debug!(%send_error, "could not report the error to the sender");
                    }
                }
                Err(error)
            }
        }
    }
}

/// Applies the trust policy to a sender. Returns its trust store entry, or `None` for an unpaired sender that is let
/// in because `allow_untrusted` is on.
async fn check_sender(
    policy: &ReceivePolicy,
    session: &PeerSession,
    device_id: DeviceId,
) -> Result<Option<TrustedDevice>> {
    // Another process (`lanshare trusted remove`, the app) may have changed the list since the last connection.
    if let Err(error) = policy.trust.refresh().await {
        warn!(%error, "could not re-read the trusted devices, using the last known list");
    }
    if let Some(device) = policy.trust.get_by_fingerprint(&session.fingerprint) {
        return Ok(Some(device));
    }
    if let Some(paired) = policy.trust.get_by_id(device_id) {
        warn!(name = %paired.name, fingerprint = %session.fingerprint, "a paired device id came with a different key");
        return Err(Error::FingerprintMismatch);
    }
    if policy.allow_untrusted {
        return Ok(None);
    }
    info!(fingerprint = %session.fingerprint, "rejecting an unpaired device");
    Err(Error::UntrustedDevice)
}

/// Closes our side of the control channel and waits briefly for the sender to close its side, so the last
/// message reaches it before the connection goes away.
async fn linger<R>(writer: &ControlWriter, recv: &mut R)
where
    R: AsyncRead + Unpin,
{
    writer.shutdown().await;
    let mut buffer = [0u8; 256];
    let _ = timeout(LINGER, async { while matches!(recv.read(&mut buffer).await, Ok(read) if read > 0) {} }).await;
}

/// Sends `Reject` with the error's code, then fails with `error`.
async fn reject<T>(writer: &ControlWriter, error: Error) -> Result<T> {
    writer.send(&Message::Reject { code: error.code() }).await?;
    Err(error)
}

/// Reads `Hello` and checks that its protocol version is one this device accepts. Returns the sender's device id and name.
async fn read_hello<R>(recv: &mut R, writer: &ControlWriter) -> Result<(DeviceId, String)>
where
    R: AsyncRead + Unpin,
{
    let message = match read_handshake(recv).await {
        // A `Hello` laid out by another version, which only the version field could be read from.
        Err(Error::VersionMismatch { theirs, .. }) => return reject_version(writer, theirs).await,
        other => other?,
    };
    match message {
        Message::Hello { protocol_version, device_id, device_name } if is_supported_version(protocol_version) => {
            Ok((device_id, device_name))
        }
        Message::Hello { protocol_version, .. } => reject_version(writer, protocol_version).await,
        other => Err(Error::protocol_violation(format!("expected Hello, got {other:?}"))),
    }
}

/// Tells the sender its version is not supported, then fails with [`Error::VersionMismatch`].
async fn reject_version<T>(writer: &ControlWriter, theirs: u16) -> Result<T> {
    reject(writer, Error::VersionMismatch { ours: PROTOCOL_VERSION, theirs }).await
}

/// What the sender asked for after `Hello`.
enum Request {
    Files(ValidOffer),
    Bench { transfer_id: Uuid, size: u64 },
}

/// One checked entry of an offer.
#[derive(Debug)]
struct ValidEntry {
    path: RelativePath,
    /// The file's chunk layout; empty for folders.
    layout: ChunkLayout,
}

/// A checked offer: safe, distinct paths and valid chunk layouts.
struct ValidOffer {
    transfer_id: Uuid,
    entries: Vec<ValidEntry>,
}

impl ValidOffer {
    fn size(&self) -> u64 {
        self.entries.iter().map(|entry| entry.layout.size()).sum()
    }

    fn file_count(&self) -> usize {
        self.entries.iter().filter(|entry| !entry.path.is_dir()).count()
    }

    /// What to call the offer: the name of its first top-level file or folder.
    fn summary_name(&self) -> String {
        self.entries.first().map(|entry| entry.path.top().to_owned()).unwrap_or_default()
    }

    /// The one top-level folder holding the whole offer, if there is one.
    fn single_folder(&self) -> Option<&str> {
        let top = self.entries.first()?.path.top();
        let prefix = format!("{top}/");
        self.entries.iter().all(|entry| entry.path.as_wire().starts_with(&prefix)).then_some(top)
    }
}

/// Reads one message while setting up the connection, failing with [`Error::ConnectionLost`] after
/// [`HANDSHAKE_TIMEOUT`].
async fn read_handshake<R: AsyncRead + Unpin>(recv: &mut R) -> Result<Message> {
    timeout(HANDSHAKE_TIMEOUT, read_message(recv)).await.map_err(|_elapsed| Error::ConnectionLost)?
}

/// Checks the `Offer` (or `BenchOffer`) that followed `Hello`, rejecting an offer that is not usable.
async fn check_offer(message: Message, writer: &ControlWriter) -> Result<Request> {
    let (transfer_id, chunk_size, entries) = match message {
        Message::Offer { transfer_id, chunk_size, entries } => (transfer_id, chunk_size, entries),
        Message::BenchOffer { transfer_id, size } => return Ok(Request::Bench { transfer_id, size }),
        other => return Err(Error::protocol_violation(format!("expected Offer, got {other:?}"))),
    };
    match validate_entries(&entries, chunk_size) {
        Ok(entries) => Ok(Request::Files(ValidOffer { transfer_id, entries })),
        Err(error) => {
            debug!(%error, "rejecting offer");
            reject(writer, error).await
        }
    }
}

/// Checks every entry: a safe path, no two alike (ignoring case, as macOS and Windows do), no file where another
/// entry needs a folder, folders of size 0, and a valid chunk layout.
fn validate_entries(entries: &[OfferEntry], chunk_size: u32) -> Result<Vec<ValidEntry>> {
    if entries.is_empty() || entries.len() > MAX_ENTRIES {
        return Err(Error::protocol_violation(format!("an offer of {} entries", entries.len())));
    }
    let mut checked = Vec::with_capacity(entries.len());
    let mut seen = HashSet::new();
    let mut folders = HashSet::new();
    for OfferEntry { relative_path, size } in entries {
        let path = sanitize_relative_path(relative_path)?;
        if path.is_dir() && *size != 0 {
            return Err(Error::protocol_violation(format!("folder {relative_path:?} with {size} bytes")));
        }
        let key = path.as_wire().trim_end_matches('/').to_lowercase();
        if !seen.insert(key.clone()) {
            return Err(Error::InvalidFileName);
        }
        // Every folder this entry lives in, e.g. `photos` and `photos/2024` for `photos/2024/a.jpg`.
        let mut prefix = String::new();
        for component in key.split('/').collect::<Vec<_>>().iter().rev().skip(1).rev() {
            prefix = if prefix.is_empty() { (*component).to_owned() } else { format!("{prefix}/{component}") };
            folders.insert(prefix.clone());
        }
        let layout = ChunkLayout::new(*size, chunk_size)?;
        checked.push(ValidEntry { path, layout });
    }
    // A file named like a folder another entry needs could never be created.
    if checked.iter().any(|entry| !entry.path.is_dir() && folders.contains(&entry.path.as_wire().to_lowercase())) {
        return Err(Error::InvalidFileName);
    }
    Ok(checked)
}

/// Where one file of an accepted offer goes, and what was received of it before.
struct Target {
    final_path: PathBuf,
    resume: Option<ResumeState>,
}

/// Finds a target for every file (`None` for folders) and checks there is room for the bytes still missing.
async fn prepare_targets(download_dir: &Path, offer: &ValidOffer) -> Result<Vec<Option<Target>>> {
    let dir = download_dir.to_owned();
    let transfer_id = offer.transfer_id;
    let plan: Vec<Option<(PathBuf, String, ChunkLayout)>> = offer
        .entries
        .iter()
        .map(|entry| {
            (!entry.path.is_dir()).then(|| (entry.path.parent_under(&dir), entry.path.name().to_owned(), entry.layout))
        })
        .collect();
    let (targets, available) = tokio::task::spawn_blocking(move || {
        let mut targets = Vec::with_capacity(plan.len());
        for (index, item) in plan.iter().enumerate() {
            let entry = u32::try_from(index).unwrap_or(u32::MAX);
            targets.push(match item {
                Some((parent, name, layout)) => Some(find_target(parent, name, transfer_id, entry, layout)?),
                None => None,
            });
        }
        check_writable(&dir).map_err(|error| refused_in(&dir, Error::Io(error)))?;
        let available = fs2::available_space(&dir).map_err(Error::Io)?;
        Ok::<_, Error>((targets, available))
    })
    .await
    .map_err(|error| Error::Io(io::Error::other(error)))??;

    let received: u64 = offer
        .entries
        .iter()
        .zip(&targets)
        .filter_map(|(entry, target)| Some(target.as_ref()?.resume.as_ref()?.received_bytes(&entry.layout)))
        .sum();
    let needed = offer.size() - received;
    if needed > available {
        info!(needed, available, "not enough free space for the offer");
        return Err(Error::InsufficientSpace { needed, available });
    }
    Ok(targets)
}

/// Fails if nothing can be created in `dir`, so the user is never asked about an offer that cannot be saved. Blocking.
fn check_writable(dir: &Path) -> io::Result<()> {
    let probe = dir.join(format!(".lanshare-probe-{}", Uuid::new_v4()));
    std::fs::File::create(&probe)?;
    std::fs::remove_file(&probe)
}

/// Turns a write the operating system refused in `folder` into [`Error::PermissionDenied`].
fn refused_in(folder: &Path, error: Error) -> Error {
    match error {
        Error::Io(io_error) if io_error.kind() == io::ErrorKind::PermissionDenied => {
            Error::PermissionDenied { folder: folder.display().to_string() }
        }
        other => other,
    }
}

/// Walks `name`, `name (1)`, ... in `dir` and returns the first candidate that either holds a resume state for this
/// entry of this transfer, or is completely free (no final file, part file or state file). Blocking.
fn find_target(dir: &Path, file_name: &str, transfer_id: Uuid, entry: u32, layout: &ChunkLayout) -> Result<Target> {
    for attempt in 0..MAX_NAME_ATTEMPTS {
        let final_path = dir.join(candidate_name(file_name, attempt));
        let part = part_path(&final_path);
        let state = state_path(&final_path);
        if let Some(saved) = ResumeState::load(&state)? {
            if saved.matches(transfer_id, entry, layout) {
                // A finished file must still be where it was saved; an unfinished one needs its part file at full
                // size. Otherwise the saved chunks are gone: start over under this name.
                let intact = match &saved.saved_as {
                    Some(saved_as) => saved_as.is_file(),
                    None => std::fs::metadata(&part).is_ok_and(|metadata| metadata.len() == layout.size()),
                };
                return Ok(Target { final_path, resume: intact.then_some(saved) });
            }
            continue;
        }
        if !final_path.exists() && !part.exists() && !state.exists() {
            return Ok(Target { final_path, resume: None });
        }
    }
    Err(Error::Io(io::Error::new(io::ErrorKind::AlreadyExists, "no free file name")))
}

/// `report.pdf` with attempt 0 is `report.pdf`, with attempt 2 it is `report (2).pdf`.
fn candidate_name(file_name: &str, attempt: u32) -> String {
    if attempt == 0 {
        return file_name.to_owned();
    }
    // A leading dot marks a hidden file (`.bashrc`), not an extension.
    match file_name.rfind('.').filter(|&dot| dot > 0) {
        Some(dot) => format!("{} ({attempt}){}", &file_name[..dot], &file_name[dot..]),
        None => format!("{file_name} ({attempt})"),
    }
}

/// Every entry of an accepted offer on its way to disk.
struct Parts {
    download_dir: PathBuf,
    /// Set when the whole offer is one folder.
    single_folder: Option<PathBuf>,
    entries: Vec<EntryPart>,
}

enum EntryPart {
    Folder { path: PathBuf, done: bool },
    File(Box<PartFile>),
}

impl Parts {
    fn new(download_dir: &Path, offer: &ValidOffer, targets: Vec<Option<Target>>) -> Self {
        let entries = offer
            .entries
            .iter()
            .zip(targets)
            .enumerate()
            .map(|(index, (entry, target))| match target {
                None => EntryPart::Folder { path: entry.path.under(download_dir), done: false },
                Some(target) => {
                    let index = u32::try_from(index).unwrap_or(u32::MAX);
                    EntryPart::File(Box::new(PartFile::new(offer.transfer_id, index, entry, target)))
                }
            })
            .collect();
        let single_folder = offer.single_folder().map(|top| download_dir.join(top));
        Self { download_dir: download_dir.to_owned(), single_folder, entries }
    }

    /// The `resume` list for `Accept`: the chunks of every file that has any.
    fn resume_list(&self) -> Vec<EntryResume> {
        self.files()
            .filter(|part| part.state.chunks.iter().any(Option::is_some))
            .map(|part| EntryResume { entry: part.entry, have: part.state.have_bitset() })
            .collect()
    }

    fn received_bytes(&self) -> u64 {
        self.files().map(|part| part.state.received_bytes(&part.layout)).sum()
    }

    /// Each file's chunk layout by entry, `None` for folders, for the data stream readers.
    fn layouts(&self) -> Arc<Vec<Option<ChunkLayout>>> {
        Arc::new(
            self.entries
                .iter()
                .map(|entry| match entry {
                    EntryPart::Folder { .. } => None,
                    EntryPart::File(part) => Some(part.layout),
                })
                .collect(),
        )
    }

    fn files(&self) -> impl Iterator<Item = &PartFile> {
        self.entries.iter().filter_map(|entry| match entry {
            EntryPart::File(part) => Some(part.as_ref()),
            EntryPart::Folder { .. } => None,
        })
    }

    fn file_mut(&mut self, entry: u32) -> Result<&mut PartFile> {
        match self.entries.get_mut(entry as usize) {
            Some(EntryPart::File(part)) => Ok(part.as_mut()),
            _ => Err(Error::protocol_violation(format!("entry {entry} is not a file"))),
        }
    }

    /// Checks and saves an entry the sender reported complete.
    async fn finish(&mut self, entry: u32, root_hash: ChunkHash) -> Result<()> {
        match self.entries.get_mut(entry as usize) {
            Some(EntryPart::Folder { path, done }) => {
                tokio::fs::create_dir_all(&path).await.map_err(|error| refused_in(path, Error::Io(error)))?;
                *done = true;
                Ok(())
            }
            Some(EntryPart::File(part)) => part.finish(root_hash).await,
            None => Err(Error::protocol_violation(format!("EntryDone for unknown entry {entry}"))),
        }
    }

    fn all_finished(&self) -> bool {
        self.entries.iter().all(|entry| match entry {
            EntryPart::Folder { done, .. } => *done,
            EntryPart::File(part) => part.saved_as().is_some(),
        })
    }

    /// Where to tell the user the transfer went: a single file or folder's path, or the download folder.
    fn saved_path(&self) -> PathBuf {
        if let Some(folder) = &self.single_folder {
            return folder.clone();
        }
        match self.entries.as_slice() {
            [EntryPart::File(part)] => part.saved_as().map_or_else(|| self.download_dir.clone(), Path::to_path_buf),
            _ => self.download_dir.clone(),
        }
    }

    /// Deletes the resume state of every file, once the whole transfer is done. Failures are only logged.
    async fn remove_states(&self) {
        let paths: Vec<PathBuf> = self.files().map(|part| part.state_path.clone()).collect();
        let _ = tokio::task::spawn_blocking(move || {
            for path in paths {
                if let Err(error) = std::fs::remove_file(&path) {
                    if error.kind() != io::ErrorKind::NotFound {
                        warn!(path = %path.display(), %error, "could not delete resume state");
                    }
                }
            }
        })
        .await;
    }
}

/// A chunk read off a data stream, being hashed on the blocking pool, on its way to being checked and written.
struct ArrivedChunk {
    entry: u32,
    index: u64,
    expected: ChunkHash,
    hashing: JoinHandle<(Vec<u8>, ChunkHash)>,
}

/// What the data phase of one connection needs besides the files.
struct DataSession<'a, C> {
    connection: &'a C,
    transfer_id: Uuid,
    writer: &'a ControlWriter,
    control: &'a TransferControl,
    progress: &'a watch::Sender<Progress>,
}

impl<C: Connection> DataSession<'_, C> {
    /// Runs the data phase until the sender's `Done`, with every entry checked and saved.
    ///
    /// Several jobs run at once: data stream readers (one task per stream) read and start hashing chunks; one loop
    /// writes them, sends acks, and saves each entry at its `EntryDone`; one reads the control channel; one tells the
    /// sender about local pauses; and a watchdog notices a silent sender. The first failure ends the phase.
    async fn receive_all<R>(&self, recv: &mut R, parts: &mut Parts) -> Result<()>
    where
        R: AsyncRead + Unpin,
    {
        let activity = Arc::new(Activity::new());
        let pause = PauseSync::new(self.control);
        let (chunk_sender, chunk_receiver) = mpsc::channel(CHUNK_QUEUE);
        let (done_sender, done_receiver) = mpsc::unbounded_channel();
        let layouts = parts.layouts();
        let control_and_storage = async {
            tokio::try_join!(
                read_control(recv, &pause, &activity, done_sender),
                store_chunks(self.writer, parts, chunk_receiver, done_receiver, self.progress),
            )
        };
        tokio::select! {
            result = control_and_storage => result.map(|_| ()),
            Err(error) = self.accept_streams(layouts, chunk_sender, Arc::clone(&activity)) => Err(error),
            Err(error) = pause.run(self.writer) => Err(error),
            Err(error) = activity.watchdog(self.control) => Err(error),
        }
    }

    /// Accepts the sender's data streams and starts a reader task for each.
    async fn accept_streams(
        &self,
        layouts: Arc<Vec<Option<ChunkLayout>>>,
        chunks: mpsc::Sender<ArrivedChunk>,
        activity: Arc<Activity>,
    ) -> Result<Infallible> {
        let mut readers = JoinSet::new();
        let mut accepted = 0;
        loop {
            tokio::select! {
                stream = self.connection.accept_data(self.transfer_id) => {
                    accepted += 1;
                    if accepted > MAX_STREAMS {
                        return Err(Error::protocol_violation("too many data streams"));
                    }
                    let reader = read_data_stream(stream?, Arc::clone(&layouts), chunks.clone(), Arc::clone(&activity));
                    readers.spawn(reader);
                }
                Some(joined) = readers.join_next() => {
                    joined.map_err(|error| Error::Io(io::Error::other(error)))??;
                }
            }
        }
    }
}

/// Reads the control channel during the data phase until `Done`, passing each `EntryDone` on to the storage loop.
async fn read_control<R>(
    recv: &mut R,
    pause: &PauseSync<'_>,
    activity: &Activity,
    entries_done: mpsc::UnboundedSender<(u32, ChunkHash)>,
) -> Result<()>
where
    R: AsyncRead + Unpin,
{
    loop {
        // No timeout here: the sender says nothing on the control channel while chunks flow.
        let message = read_message(recv).await?;
        activity.touch();
        match message {
            Message::EntryDone { entry, root_hash } => {
                // Fails only if the storage loop already stopped, and then its error ends the phase.
                let _ = entries_done.send((entry, root_hash));
            }
            // Dropping `entries_done` here tells the storage loop that no more entries will finish.
            Message::Done => return Ok(()),
            Message::Pause => pause.apply_from_peer(true),
            Message::Resume => pause.apply_from_peer(false),
            Message::Error { code, detail } => return Err(peer_error(code, detail.as_deref())),
            other => return Err(Error::protocol_violation(format!("unexpected {other:?} on the control channel"))),
        }
    }
}

/// Reads `ChunkHeader` frames and chunk bytes from one data stream, until the sender closes it.
async fn read_data_stream(
    mut stream: RecvHalf,
    layouts: Arc<Vec<Option<ChunkLayout>>>,
    chunks: mpsc::Sender<ArrivedChunk>,
    activity: Arc<Activity>,
) -> Result<()> {
    while let Some(message) = read_message_or_end(&mut stream).await? {
        activity.touch();
        let Message::ChunkHeader { entry, index, len, hash } = message else {
            return Err(Error::protocol_violation(format!("unexpected {message:?} on a data stream")));
        };
        let layout = layouts.get(entry as usize).copied().flatten();
        if !layout.is_some_and(|layout| index < layout.count() && len == layout.len(index)) {
            let detail = format!("chunk {index} of entry {entry} with {len} bytes does not fit the offer");
            return Err(Error::protocol_violation(detail));
        }
        let mut data = vec![0; len as usize];
        read_exact_tracked(&mut stream, &mut data, &activity).await?;
        let hashing = tokio::task::spawn_blocking(move || {
            let hash = hash_chunk(&data);
            (data, hash)
        });
        if chunks.send(ArrivedChunk { entry, index, expected: hash, hashing }).await.is_err() {
            // The data phase is over; nobody needs more chunks.
            break;
        }
    }
    Ok(())
}

/// Writes each arrived chunk whose hash matches and answers `ChunkAck`, or `ChunkNack` on a mismatch; saves each
/// entry when its `EntryDone` arrives. Returns once the sender's `Done` came and every entry is saved.
async fn store_chunks(
    writer: &ControlWriter,
    parts: &mut Parts,
    mut chunks: mpsc::Receiver<ArrivedChunk>,
    mut entries_done: mpsc::UnboundedReceiver<(u32, ChunkHash)>,
    progress: &watch::Sender<Progress>,
) -> Result<()> {
    let mut mismatches: HashMap<(u32, u64), u32> = HashMap::new();
    let mut readers_open = true;
    loop {
        tokio::select! {
            chunk = chunks.recv(), if readers_open => match chunk {
                Some(chunk) => store_chunk(writer, parts, chunk, &mut mismatches, progress).await?,
                None => readers_open = false,
            },
            done = entries_done.recv() => match done {
                Some((entry, root_hash)) => parts.finish(entry, root_hash).await?,
                None if parts.all_finished() => return Ok(()),
                None => return Err(Error::protocol_violation("Done before every entry was finished")),
            },
        }
    }
}

async fn store_chunk(
    writer: &ControlWriter,
    parts: &mut Parts,
    chunk: ArrivedChunk,
    mismatches: &mut HashMap<(u32, u64), u32>,
    progress: &watch::Sender<Progress>,
) -> Result<()> {
    let ArrivedChunk { entry, index, expected, hashing } = chunk;
    let (data, actual) = hashing.await.map_err(|error| Error::Io(io::Error::other(error)))?;
    let len = data.len() as u64;
    if actual == expected {
        parts.file_mut(entry)?.write_chunk(index, data, actual).await?;
        writer.send(&Message::ChunkAck { entry, index }).await?;
        progress.send_modify(|progress| {
            progress.transferred += len;
            progress.current_entry = entry;
        });
        return Ok(());
    }
    let count = mismatches.entry((entry, index)).or_default();
    *count += 1;
    warn!(entry, index, attempt = *count, "chunk failed its hash check");
    if *count >= MAX_HASH_ATTEMPTS {
        return Err(Error::HashMismatch);
    }
    writer.send(&Message::ChunkNack { entry, index }).await
}

/// One file of a transfer: a `.lanshare-part` file being filled, with its resume state.
///
/// The part file is only opened when the first chunk arrives, and closed once the file is saved, so a folder of
/// thousands of small files never has them all open. If it is dropped before [`PartFile::finish`] (on an error, a
/// lost connection, or a cancelled future), `Drop` syncs the data and saves the state, so the transfer can resume.
/// `Drop` works like a `defer` in Go.
struct PartFile {
    entry: u32,
    dir: PathBuf,
    file_name: String,
    final_path: PathBuf,
    part_path: PathBuf,
    state_path: PathBuf,
    layout: ChunkLayout,
    state: ResumeState,
    /// Open while chunks are being written.
    writer: Option<ChunkWriter>,
    unsaved: u32,
    /// The checkpoint running in the background, if any.
    checkpoint_task: Option<JoinHandle<Result<()>>>,
}

impl PartFile {
    fn new(transfer_id: Uuid, entry: u32, valid: &ValidEntry, target: Target) -> Self {
        let state = target.resume.unwrap_or_else(|| ResumeState::new(transfer_id, entry, &valid.layout));
        Self {
            entry,
            dir: target.final_path.parent().map(Path::to_path_buf).unwrap_or_default(),
            file_name: valid.path.name().to_owned(),
            part_path: part_path(&target.final_path),
            state_path: state_path(&target.final_path),
            final_path: target.final_path,
            layout: valid.layout,
            state,
            writer: None,
            unsaved: 0,
            checkpoint_task: None,
        }
    }

    /// Where the finished file was saved, once it is.
    fn saved_as(&self) -> Option<&Path> {
        self.state.saved_as.as_deref()
    }

    /// The open part file, creating its folder and the file itself first if needed.
    async fn writer(&mut self) -> Result<ChunkWriter> {
        if let Some(writer) = &self.writer {
            return Ok(writer.clone());
        }
        tokio::fs::create_dir_all(&self.dir).await.map_err(|error| refused_in(&self.dir, Error::Io(error)))?;
        let writer = ChunkWriter::open(&self.part_path, self.layout.size())
            .await
            .map_err(|error| refused_in(&self.dir, error))?;
        self.writer = Some(writer.clone());
        if self.layout.count() > 1 {
            // Saved right away, so the transfer can be found again even if it stops before the first checkpoint.
            // A file of one chunk is saved at its end anyway, which saves a write per file in large folders.
            self.start_checkpoint().await?;
            self.wait_for_checkpoint().await?;
        }
        Ok(writer)
    }

    /// Writes a verified chunk, saving the state after every few.
    async fn write_chunk(&mut self, index: u64, data: Vec<u8>, hash: ChunkHash) -> Result<()> {
        if self.saved_as().is_some() {
            return Err(Error::protocol_violation(format!("chunk for entry {}, which is already saved", self.entry)));
        }
        let writer = self.writer().await?;
        writer.write_chunk(self.layout.range(index).start, data).await?;
        self.state.mark(index, hash);
        self.unsaved += 1;
        if self.unsaved >= SAVE_EVERY_CHUNKS {
            self.start_checkpoint().await?;
        }
        Ok(())
    }

    /// Starts a checkpoint in the background, once the previous one is done: syncs the data to disk, then saves a
    /// snapshot of the state. The snapshot only lists chunks written before the sync started, so the state never
    /// lists a chunk that is not really on disk. Meanwhile, new chunks keep arriving.
    async fn start_checkpoint(&mut self) -> Result<()> {
        self.wait_for_checkpoint().await?;
        let (writer, state, path) = (self.writer.clone(), self.state.clone(), self.state_path.clone());
        self.checkpoint_task = Some(tokio::task::spawn_blocking(move || {
            if let Some(writer) = writer {
                writer.sync_blocking()?;
            }
            state.save(&path)
        }));
        self.unsaved = 0;
        Ok(())
    }

    /// Waits for the background checkpoint, if one is running, and returns its error.
    async fn wait_for_checkpoint(&mut self) -> Result<()> {
        match self.checkpoint_task.take() {
            Some(task) => task.await.map_err(|error| Error::Io(io::Error::other(error)))?,
            None => Ok(()),
        }
    }

    /// Checks the root hash, renames the part file to a free final name, and closes it. The state is kept (marking
    /// the file as saved) until the whole transfer is done. A file saved on an earlier connection is only checked.
    async fn finish(&mut self, root_hash: ChunkHash) -> Result<()> {
        match self.state.root_hash() {
            None => return Err(Error::protocol_violation(format!("EntryDone before entry {} arrived", self.entry))),
            Some(root) if root != root_hash => {
                warn!(path = %self.part_path.display(), "root hash mismatch, keeping the partial file");
                return Err(Error::HashMismatch);
            }
            Some(_) => {}
        }
        if self.saved_as().is_some() {
            return Ok(());
        }
        // An empty file has no chunks, so its part file is created here.
        let writer = self.writer().await?;
        // A checkpoint still running could otherwise write an older state after this one.
        self.wait_for_checkpoint().await?;
        writer.sync().await?;
        let (dir, file_name, preferred) = (self.dir.clone(), self.file_name.clone(), self.final_path.clone());
        let part_path = self.part_path.clone();
        let mut state = self.state.clone();
        let state_path = self.state_path.clone();
        let saved = tokio::task::spawn_blocking(move || {
            let path = free_final_path(&dir, &file_name, preferred)?;
            std::fs::rename(&part_path, &path)?;
            state.saved_as = Some(path);
            state.save(&state_path).map_err(|error| io::Error::other(error.to_string()))?;
            Ok::<_, io::Error>(state)
        })
        .await
        .map_err(|error| Error::Io(io::Error::other(error)))?
        .map_err(|error| refused_in(&self.dir, Error::Io(error)))?;
        self.state = saved;
        self.writer = None;
        Ok(())
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        // Nothing to keep for a file that was saved, or never opened.
        let Some(writer) = &self.writer else { return };
        // `Drop` cannot be async, so this blocks briefly. It runs once per stopped file. A background checkpoint may
        // still be running; it saves an older snapshot under its own temporary name, so whichever lands last, the
        // state stays valid (at worst a few chunks are sent again).
        let saved = writer.sync_blocking().and_then(|()| self.state.save(&self.state_path));
        match saved {
            Ok(()) => debug!(path = %self.state_path.display(), "saved resume state"),
            Err(error) => warn!(path = %self.state_path.display(), %error, "could not save resume state"),
        }
    }
}

/// `preferred` if it is still free, otherwise the first free `name (n).ext`. Existing files are never overwritten.
fn free_final_path(dir: &Path, file_name: &str, preferred: PathBuf) -> io::Result<PathBuf> {
    if !preferred.exists() {
        return Ok(preferred);
    }
    (1..MAX_NAME_ATTEMPTS)
        .map(|attempt| dir.join(candidate_name(file_name, attempt)))
        .find(|path| !path.exists())
        .ok_or_else(|| io::Error::new(io::ErrorKind::AlreadyExists, "no free file name"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_names_number_before_extension() {
        assert_eq!(candidate_name("report.pdf", 0), "report.pdf");
        assert_eq!(candidate_name("report.pdf", 1), "report (1).pdf");
        assert_eq!(candidate_name("archive.tar.gz", 2), "archive.tar (2).gz");
        assert_eq!(candidate_name("README", 3), "README (3)");
        assert_eq!(candidate_name(".bashrc", 1), ".bashrc (1)");
    }

    #[test]
    fn target_skips_taken_names_and_finds_its_own_state() {
        let dir = tempfile::tempdir().unwrap();
        let layout = ChunkLayout::new(10, 4).unwrap();
        let id = Uuid::new_v4();
        std::fs::write(dir.path().join("a.txt"), b"old").unwrap();

        let target = find_target(dir.path(), "a.txt", id, 0, &layout).unwrap();
        assert_eq!(target.final_path, dir.path().join("a (1).txt"));
        assert!(target.resume.is_none());

        // Another transfer is in progress under `a (1).txt`; ours was saved under `a (2).txt`.
        ResumeState::new(Uuid::new_v4(), 0, &layout).save(&dir.path().join("a (1).txt.lanshare-state")).unwrap();
        ResumeState::new(id, 0, &layout).save(&dir.path().join("a (2).txt.lanshare-state")).unwrap();
        std::fs::write(dir.path().join("a (2).txt.lanshare-part"), [0; 10]).unwrap();
        let target = find_target(dir.path(), "a.txt", id, 0, &layout).unwrap();
        assert_eq!(target.final_path, dir.path().join("a (2).txt"));
        assert!(target.resume.is_some());
    }

    #[test]
    fn final_name_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let preferred = dir.path().join("a.txt");
        assert_eq!(free_final_path(dir.path(), "a.txt", preferred.clone()).unwrap(), preferred);
        std::fs::write(&preferred, b"appeared meanwhile").unwrap();
        assert_eq!(free_final_path(dir.path(), "a.txt", preferred).unwrap(), dir.path().join("a (1).txt"));
    }

    fn entry(path: &str, size: u64) -> OfferEntry {
        OfferEntry { relative_path: path.into(), size }
    }

    #[test]
    fn offers_with_folders_are_checked() {
        let good = [entry("Photos/a.jpg", 3), entry("Photos/empty/", 0), entry("notes.txt", 1)];
        let checked = validate_entries(&good, 4).unwrap();
        assert_eq!(checked.len(), 3);
        assert!(checked[1].path.is_dir());

        let invalid = |entries: &[OfferEntry]| validate_entries(entries, 4).unwrap_err().code();
        assert_eq!(invalid(&[entry("../x", 1)]), ErrorCode::InvalidFileName);
        assert_eq!(invalid(&[entry("CON/x", 1)]), ErrorCode::InvalidFileName);
        assert_eq!(invalid(&[entry("a.txt", 1), entry("A.TXT", 1)]), ErrorCode::InvalidFileName, "same name");
        assert_eq!(invalid(&[entry("a", 1), entry("a/b", 1)]), ErrorCode::InvalidFileName, "file in the way");
        assert_eq!(invalid(&[entry("dir/", 5)]), ErrorCode::ProtocolViolation, "folder with data");
        assert_eq!(invalid(&[]), ErrorCode::ProtocolViolation);
    }
}
