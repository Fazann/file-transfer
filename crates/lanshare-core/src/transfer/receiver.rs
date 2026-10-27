//! Receiving side: checks the hello and offer, resumes from a `.lanshare-state` file when the same transfer comes
//! back, verifies every chunk arriving on the data streams, and saves the file once its root hash matches.

use std::{
    collections::HashMap,
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
    peer_error, read_exact_tracked, read_frame, sanitize_file_name,
    scheduler::MAX_STREAMS,
    state::{part_path, state_path, ResumeState},
    writer::ChunkWriter,
    Progress, SAVE_EVERY_CHUNKS,
};
use crate::{
    error::{Error, ErrorCode, Result},
    identity::DeviceId,
    protocol::{read_message, read_message_or_end, EntryResume, Message, OfferEntry, PROTOCOL_VERSION},
    transport::{Connection, RecvHalf},
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

/// An offer received from a sender, after its file name was sanitized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingOffer {
    /// Stable id of the sending device.
    pub device_id: DeviceId,
    /// Name the sender reported for its device.
    pub device_name: String,
    /// Sanitized file name.
    pub file_name: String,
    /// File size in bytes.
    pub size: u64,
}

/// Decides whether to accept an incoming offer. The CLI asks on the terminal; the app will ask in the UI.
///
/// The method is written as "returns a `Future`" instead of `async fn` so we can require the future to be `Send`
/// (safe to move between threads). Implementors can still write a plain `async fn decide(...)`.
pub trait OfferDecider: Send + Sync {
    /// Returns `true` to accept the offer.
    fn decide(&self, offer: &IncomingOffer) -> impl Future<Output = bool> + Send;
}

/// A file that was received and saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedFile {
    /// Where the file was saved. May differ from the offered name if that name was taken.
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
}

/// Handles one incoming transfer on `connection`, saving the file into `download_dir`.
///
/// Data is written to `<name>.lanshare-part` with resume state in `<name>.lanshare-state`. If the transfer stops
/// (error, dropped connection, or this future is dropped), both files are kept, and when the sender offers the same
/// transfer again it continues from the chunks already received, without asking `decider` again. The finished file
/// gets the offered name, or `name (1).ext` and so on if that is taken. `control` pauses and resumes the transfer
/// from this side.
///
/// # Errors
///
/// - [`Error::VersionMismatch`], [`Error::InvalidFileName`], [`Error::InsufficientSpace`] (the sender is told with
///   `Reject`)
/// - [`Error::HashMismatch`] if a chunk fails its hash three times or the file fails its root hash
/// - [`Error::ProtocolViolation`] if messages arrive out of order or do not fit the offer
/// - [`Error::ConnectionLost`] if the sender disconnects or goes quiet for [`IDLE_TIMEOUT`](super::IDLE_TIMEOUT)
/// - [`Error::Io`] if the file cannot be created or written
pub async fn receive_file<C, D>(
    connection: &C,
    download_dir: &Path,
    decider: &D,
    control: &TransferControl,
    progress: &watch::Sender<Progress>,
) -> Result<ReceiveOutcome>
where
    C: Connection,
    D: OfferDecider,
{
    let (send, mut recv) = connection.open_control().await?;
    let writer = ControlWriter::new(send);
    let result = handle_request(connection, &mut recv, &writer, download_dir, decider, control, progress).await;
    // Whatever the outcome, the last message (`Done`, `Reject` or `Error`) must reach the sender before the
    // connection is dropped: dropping a QUIC connection discards data that is still on its way.
    linger(&writer, &mut recv).await;
    result
}

/// Everything after the control channel is open: hello, offer, and the transfer itself.
async fn handle_request<C, D, R>(
    connection: &C,
    recv: &mut R,
    writer: &ControlWriter,
    download_dir: &Path,
    decider: &D,
    control: &TransferControl,
    progress: &watch::Sender<Progress>,
) -> Result<ReceiveOutcome>
where
    C: Connection,
    D: OfferDecider,
    R: AsyncRead + Unpin,
{
    let (device_id, device_name) = read_hello(recv, writer).await?;
    let offer = match read_offer(recv, writer).await? {
        Request::File(offer) => offer,
        Request::Bench { transfer_id, size } => {
            let bytes = receive_bench(connection, recv, writer, transfer_id, size).await?;
            return Ok(ReceiveOutcome::Benchmark { device_name, bytes });
        }
    };
    let incoming = IncomingOffer { device_id, device_name, file_name: offer.file_name.clone(), size: offer.size() };

    let target = match prepare_target(download_dir, &offer).await {
        Ok(target) => target,
        Err(error) => return reject(writer, error).await,
    };
    // A transfer we are resuming was accepted before; only new transfers ask.
    if target.resume.is_none() && !decider.decide(&incoming).await {
        info!(file_name = %incoming.file_name, "offer declined");
        writer.send(&Message::Reject { code: ErrorCode::Declined }).await?;
        return Ok(ReceiveOutcome::Declined(incoming));
    }
    let mut part = match PartFile::open(download_dir, &offer, target).await {
        Ok(part) => part,
        Err(error) => return reject(writer, error).await,
    };
    // From here on, `part` saves the resume state if anything fails or this future is dropped.

    writer.send(&Message::Accept { resume: part.resume_list() }).await?;
    let resumed = part.state.received_bytes(&part.layout);
    info!(path = %part.part_path.display(), size = offer.size(), resumed, "receiving");
    progress.send_replace(Progress { transferred: resumed, total: offer.size(), resumed });

    let session = DataSession { connection, transfer_id: offer.transfer_id, writer, control, progress };
    match session.receive_and_save(recv, &mut part).await {
        Ok(path) => {
            info!(path = %path.display(), size = offer.size(), "file received");
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

/// Reads `Hello` and checks the protocol version. Returns the sender's device id and name.
async fn read_hello<R>(recv: &mut R, writer: &ControlWriter) -> Result<(DeviceId, String)>
where
    R: AsyncRead + Unpin,
{
    let message = match read_frame(recv).await {
        // A `Hello` laid out by another version, which only the version field could be read from.
        Err(Error::VersionMismatch { theirs, .. }) => return reject_version(writer, theirs).await,
        other => other?,
    };
    match message {
        Message::Hello { protocol_version, device_id, device_name } if protocol_version == PROTOCOL_VERSION => {
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
    File(ValidOffer),
    Bench { transfer_id: Uuid, size: u64 },
}

/// A checked offer: one entry with a safe name and a valid chunk layout.
struct ValidOffer {
    transfer_id: Uuid,
    file_name: String,
    layout: ChunkLayout,
}

impl ValidOffer {
    fn size(&self) -> u64 {
        self.layout.size()
    }
}

/// Reads `Offer` (or `BenchOffer`) and checks it, rejecting an offer that is not usable.
async fn read_offer<R>(recv: &mut R, writer: &ControlWriter) -> Result<Request>
where
    R: AsyncRead + Unpin,
{
    let (transfer_id, chunk_size, entries) = match read_frame(recv).await? {
        Message::Offer { transfer_id, chunk_size, entries } => (transfer_id, chunk_size, entries),
        Message::BenchOffer { transfer_id, size } => return Ok(Request::Bench { transfer_id, size }),
        other => return Err(Error::protocol_violation(format!("expected Offer, got {other:?}"))),
    };
    // Exactly one entry until phase 7 adds folders.
    let [OfferEntry { relative_path, size }] = entries.as_slice() else {
        let error = Error::protocol_violation(format!("expected one entry, got {}", entries.len()));
        return reject(writer, error).await;
    };
    let file_name = match sanitize_file_name(relative_path) {
        Ok(file_name) => file_name,
        Err(error) => {
            debug!(?relative_path, "rejecting unsafe file name");
            return reject(writer, error).await;
        }
    };
    match ChunkLayout::new(*size, chunk_size) {
        Ok(layout) => Ok(Request::File(ValidOffer { transfer_id, file_name, layout })),
        Err(error) => reject(writer, error).await,
    }
}

/// Where an accepted file will go, and the state to resume from if this transfer was seen before.
struct Target {
    final_path: PathBuf,
    resume: Option<ResumeState>,
}

/// Finds the target for the offer and checks there is room for the bytes still missing.
async fn prepare_target(download_dir: &Path, offer: &ValidOffer) -> Result<Target> {
    let dir = download_dir.to_owned();
    let file_name = offer.file_name.clone();
    let (transfer_id, layout) = (offer.transfer_id, offer.layout);
    let (target, available) = tokio::task::spawn_blocking(move || {
        let target = find_target(&dir, &file_name, transfer_id, &layout)?;
        let available = fs2::available_space(&dir).map_err(Error::Io)?;
        Ok::<_, Error>((target, available))
    })
    .await
    .map_err(|error| Error::Io(io::Error::other(error)))??;

    let received = target.resume.as_ref().map_or(0, |state| state.received_bytes(&offer.layout));
    let needed = offer.size() - received;
    if needed > available {
        info!(needed, available, "not enough free space for the offer");
        return Err(Error::InsufficientSpace { needed, available });
    }
    Ok(target)
}

/// Walks `name`, `name (1)`, ... and returns the first candidate that either holds a resume state for this transfer
/// or is completely free (no final file, part file or state file). Blocking.
fn find_target(dir: &Path, file_name: &str, transfer_id: Uuid, layout: &ChunkLayout) -> Result<Target> {
    for attempt in 0..MAX_NAME_ATTEMPTS {
        let final_path = dir.join(candidate_name(file_name, attempt));
        let part = part_path(&final_path);
        let state = state_path(&final_path);
        if let Some(saved) = ResumeState::load(&state)? {
            if saved.matches(transfer_id, layout) {
                // Without the part file at full size the saved chunks are gone; start over under this name.
                let part_intact = std::fs::metadata(&part).is_ok_and(|metadata| metadata.len() == layout.size());
                return Ok(Target { final_path, resume: part_intact.then_some(saved) });
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

/// A chunk read off a data stream, being hashed on the blocking pool, on its way to being checked and written.
struct ArrivedChunk {
    index: u64,
    expected: ChunkHash,
    /// Gives back the chunk's bytes and their actual hash.
    hashing: JoinHandle<(Vec<u8>, ChunkHash)>,
}

/// What the data phase of one connection needs besides the part file.
struct DataSession<'a, C> {
    connection: &'a C,
    transfer_id: Uuid,
    writer: &'a ControlWriter,
    control: &'a TransferControl,
    progress: &'a watch::Sender<Progress>,
}

impl<C: Connection> DataSession<'_, C> {
    /// Receives the chunks, checks the root hash, saves the file, and confirms with `Done`.
    async fn receive_and_save<R>(&self, recv: &mut R, part: &mut PartFile) -> Result<PathBuf>
    where
        R: AsyncRead + Unpin,
    {
        let sender_root = self.receive_chunks(recv, part).await?;
        match part.state.root_hash() {
            None => return Err(Error::protocol_violation("EntryDone before every chunk was received")),
            Some(root) if root != sender_root => {
                warn!(path = %part.part_path.display(), "root hash mismatch, keeping the partial file");
                return Err(Error::HashMismatch);
            }
            Some(_) => {}
        }
        let path = part.finish().await?;
        loop {
            match read_frame(recv).await? {
                Message::Done => break,
                // A late pause or resume no longer matters.
                Message::Pause | Message::Resume => {}
                other => return Err(Error::protocol_violation(format!("expected Done, got {other:?}"))),
            }
        }
        self.writer.send(&Message::Done).await?;
        Ok(path)
    }

    /// Runs the data phase until the sender's `EntryDone`, and returns the sender's root hash.
    ///
    /// Several jobs run at once: data stream readers (one task per stream) read and start hashing chunks, one loop
    /// writes them and sends acks, one reads the control channel, one tells the sender about local pauses, and a
    /// watchdog notices a silent sender. The first to finish or fail ends the phase.
    async fn receive_chunks<R>(&self, recv: &mut R, part: &mut PartFile) -> Result<ChunkHash>
    where
        R: AsyncRead + Unpin,
    {
        let activity = Arc::new(Activity::new());
        let pause = PauseSync::new(self.control);
        let (chunk_sender, chunk_receiver) = mpsc::channel(CHUNK_QUEUE);
        let layout = part.layout;
        tokio::select! {
            root_hash = read_control(recv, &pause, &activity) => root_hash,
            Err(error) = self.accept_streams(layout, chunk_sender, Arc::clone(&activity)) => Err(error),
            Err(error) = verify_chunks(self.writer, part, chunk_receiver, self.progress) => Err(error),
            Err(error) = pause.run(self.writer) => Err(error),
            Err(error) = activity.watchdog(self.control) => Err(error),
        }
    }

    /// Accepts the sender's data streams and starts a reader task for each.
    async fn accept_streams(
        &self,
        layout: ChunkLayout,
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
                    readers.spawn(read_data_stream(stream?, layout, chunks.clone(), Arc::clone(&activity)));
                }
                Some(joined) = readers.join_next() => {
                    joined.map_err(|error| Error::Io(io::Error::other(error)))??;
                }
            }
        }
    }
}

/// Reads the control channel during the data phase until `EntryDone`, and returns the sender's root hash.
async fn read_control<R>(recv: &mut R, pause: &PauseSync<'_>, activity: &Activity) -> Result<ChunkHash>
where
    R: AsyncRead + Unpin,
{
    loop {
        // No timeout here: the sender says nothing on the control channel while chunks flow.
        let message = read_message(recv).await?;
        activity.touch();
        match message {
            Message::EntryDone { entry: 0, root_hash } => return Ok(root_hash),
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
    layout: ChunkLayout,
    chunks: mpsc::Sender<ArrivedChunk>,
    activity: Arc<Activity>,
) -> Result<()> {
    while let Some(message) = read_message_or_end(&mut stream).await? {
        activity.touch();
        let Message::ChunkHeader { entry: 0, index, len, hash } = message else {
            return Err(Error::protocol_violation(format!("unexpected {message:?} on a data stream")));
        };
        if index >= layout.count() || len != layout.len(index) {
            return Err(Error::protocol_violation(format!("chunk {index} with {len} bytes does not fit the offer")));
        }
        let mut data = vec![0; len as usize];
        read_exact_tracked(&mut stream, &mut data, &activity).await?;
        let hashing = tokio::task::spawn_blocking(move || {
            let hash = hash_chunk(&data);
            (data, hash)
        });
        if chunks.send(ArrivedChunk { index, expected: hash, hashing }).await.is_err() {
            // The data phase is over; nobody needs more chunks.
            break;
        }
    }
    Ok(())
}

/// Writes each arrived chunk whose hash matches and answers `ChunkAck`, or `ChunkNack` on a mismatch.
///
/// Only returns on an error: the data phase ends when the control channel delivers `EntryDone`.
async fn verify_chunks(
    writer: &ControlWriter,
    part: &mut PartFile,
    mut chunks: mpsc::Receiver<ArrivedChunk>,
    progress: &watch::Sender<Progress>,
) -> Result<Infallible> {
    let mut mismatches: HashMap<u64, u32> = HashMap::new();
    while let Some(ArrivedChunk { index, expected, hashing }) = chunks.recv().await {
        let (data, actual) = hashing.await.map_err(|error| Error::Io(io::Error::other(error)))?;
        let len = data.len() as u64;
        if actual == expected {
            part.write_chunk(index, data, actual).await?;
            writer.send(&Message::ChunkAck { entry: 0, index }).await?;
            progress.send_modify(|progress| progress.transferred += len);
            continue;
        }
        let count = mismatches.entry(index).or_default();
        *count += 1;
        warn!(index, attempt = *count, "chunk failed its hash check");
        if *count >= MAX_HASH_ATTEMPTS {
            return Err(Error::HashMismatch);
        }
        writer.send(&Message::ChunkNack { entry: 0, index }).await?;
    }
    // Every reader stopped, which only happens once the phase is ending anyway.
    Err(Error::Cancelled)
}

/// A `.lanshare-part` file being filled, with its resume state.
///
/// If it is dropped before [`PartFile::finish`] (on an error, a lost connection, or a cancelled future), `Drop`
/// syncs the data and saves the state, so the transfer can resume. `Drop` works like a `defer` in Go.
struct PartFile {
    dir: PathBuf,
    file_name: String,
    final_path: PathBuf,
    part_path: PathBuf,
    state_path: PathBuf,
    layout: ChunkLayout,
    state: ResumeState,
    writer: ChunkWriter,
    unsaved: u32,
    /// The checkpoint running in the background, if any.
    checkpoint_task: Option<JoinHandle<Result<()>>>,
    finished: bool,
}

impl PartFile {
    async fn open(dir: &Path, offer: &ValidOffer, target: Target) -> Result<Self> {
        let part_path = part_path(&target.final_path);
        let writer = ChunkWriter::open(&part_path, offer.size()).await?;
        let state = target.resume.unwrap_or_else(|| ResumeState::new(offer.transfer_id, &offer.layout));
        let mut part = Self {
            dir: dir.to_owned(),
            file_name: offer.file_name.clone(),
            state_path: state_path(&target.final_path),
            final_path: target.final_path,
            part_path,
            layout: offer.layout,
            state,
            writer,
            unsaved: 0,
            checkpoint_task: None,
            finished: false,
        };
        // Saved right away, so the transfer can be found again even if it stops before the first checkpoint.
        part.start_checkpoint().await?;
        part.wait_for_checkpoint().await?;
        Ok(part)
    }

    /// The `resume` list for `Accept`: empty when nothing was received yet.
    fn resume_list(&self) -> Vec<EntryResume> {
        if self.state.chunks.iter().any(Option::is_some) {
            vec![EntryResume { entry: 0, have: self.state.have_bitset() }]
        } else {
            Vec::new()
        }
    }

    /// Writes a verified chunk, saving the state after every few. Returns the buffer for reuse.
    async fn write_chunk(&mut self, index: u64, data: Vec<u8>, hash: ChunkHash) -> Result<Vec<u8>> {
        let data = self.writer.write_chunk(self.layout.range(index).start, data).await?;
        self.state.mark(index, hash);
        self.unsaved += 1;
        if self.unsaved >= SAVE_EVERY_CHUNKS {
            self.start_checkpoint().await?;
        }
        Ok(data)
    }

    /// Starts a checkpoint in the background, once the previous one is done: syncs the data to disk, then saves a
    /// snapshot of the state. The snapshot only lists chunks written before the sync started, so the state never
    /// lists a chunk that is not really on disk. Meanwhile, new chunks keep arriving.
    async fn start_checkpoint(&mut self) -> Result<()> {
        self.wait_for_checkpoint().await?;
        let (writer, state, path) = (self.writer.clone(), self.state.clone(), self.state_path.clone());
        self.checkpoint_task = Some(tokio::task::spawn_blocking(move || {
            writer.sync_blocking()?;
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

    /// Renames the part file to a free final name and deletes the state. Returns the final path.
    async fn finish(&mut self) -> Result<PathBuf> {
        // A checkpoint still running could otherwise write the state file again after it was deleted.
        self.wait_for_checkpoint().await?;
        self.writer.sync().await?;
        let (dir, file_name, preferred) = (self.dir.clone(), self.file_name.clone(), self.final_path.clone());
        let (part_path, state_path) = (self.part_path.clone(), self.state_path.clone());
        let path = tokio::task::spawn_blocking(move || {
            let path = free_final_path(&dir, &file_name, preferred)?;
            std::fs::rename(&part_path, &path)?;
            if let Err(error) = std::fs::remove_file(&state_path) {
                warn!(path = %state_path.display(), %error, "could not delete resume state");
            }
            Ok::<_, io::Error>(path)
        })
        .await
        .map_err(|error| Error::Io(io::Error::other(error)))?
        .map_err(Error::Io)?;
        self.finished = true;
        Ok(path)
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // `Drop` cannot be async, so this blocks briefly. It runs once per stopped transfer. A background checkpoint
        // may still be running; it saves an older snapshot under its own temporary name, so whichever lands last,
        // the state stays valid (at worst a few chunks are sent again).
        let saved = self.writer.sync_blocking().and_then(|()| self.state.save(&self.state_path));
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

        let target = find_target(dir.path(), "a.txt", id, &layout).unwrap();
        assert_eq!(target.final_path, dir.path().join("a (1).txt"));
        assert!(target.resume.is_none());

        // Another transfer is in progress under `a (1).txt`; ours was saved under `a (2).txt`.
        ResumeState::new(Uuid::new_v4(), &layout).save(&dir.path().join("a (1).txt.lanshare-state")).unwrap();
        ResumeState::new(id, &layout).save(&dir.path().join("a (2).txt.lanshare-state")).unwrap();
        std::fs::write(dir.path().join("a (2).txt.lanshare-part"), [0; 10]).unwrap();
        let target = find_target(dir.path(), "a.txt", id, &layout).unwrap();
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
}
