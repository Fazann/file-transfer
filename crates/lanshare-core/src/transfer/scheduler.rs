//! Parallel chunk sending. Missing chunks wait in one shared queue and each data stream has a worker that pulls the
//! next chunk, so faster streams do more of the work. Acks and nacks come back on the control channel; a nacked
//! chunk goes back into the queue.

use std::{
    collections::HashMap,
    fs::File,
    io,
    mem::take,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncWriteExt},
    sync::{watch, Semaphore},
    task::JoinSet,
};
use tracing::debug;
use uuid::Uuid;

use super::{
    chunk::{read_and_hash, ChunkHash, ChunkLayout},
    control::{Activity, ControlWriter, PauseSync, TransferControl},
    hash_cache::HashCache,
    peer_error, write_all_limited, write_frame, Progress,
};
use crate::{
    error::{Error, Result},
    protocol::{read_message, Message},
    transport::{Connection, SendHalf},
};

/// Data streams per transfer unless `config.toml` says otherwise.
pub const DEFAULT_STREAMS: usize = 4;

/// Most data streams per transfer.
pub const MAX_STREAMS: usize = 16;

/// Chunks each stream may have sent without an answer. Bounds how far the sender runs ahead, so nacks are handled
/// while the chunk is still fresh.
const IN_FLIGHT_PER_STREAM: usize = 4;

/// How long idle workers get to close their streams once every chunk is acknowledged.
const WORKER_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Reads chunks of the file being sent. Cheap to clone: the file is shared.
#[derive(Debug, Clone)]
pub(crate) struct ChunkSource {
    /// `std::fs::File` in an `Arc`, so reads can run in `spawn_blocking` with positional reads.
    file: Arc<File>,
    layout: ChunkLayout,
}

impl ChunkSource {
    pub(crate) fn new(file: Arc<File>, layout: ChunkLayout) -> Self {
        Self { file, layout }
    }

    pub(crate) fn layout(&self) -> &ChunkLayout {
        &self.layout
    }

    /// Reads chunk `index` into `buffer` and hashes it, on the blocking pool. Returns the filled buffer and the hash.
    ///
    /// With several workers reading at once, chunks are hashed on several CPU cores in parallel.
    pub(crate) async fn read(&self, index: u64, mut buffer: Vec<u8>) -> Result<(Vec<u8>, ChunkHash)> {
        let source = self.clone();
        tokio::task::spawn_blocking(move || {
            let hash = read_and_hash(&source.file, &source.layout, index, &mut buffer).map_err(Error::Io)?;
            Ok((buffer, hash))
        })
        .await
        .map_err(|error| Error::Io(io::Error::other(error)))?
    }
}

/// What one session needs to send its missing chunks.
pub(crate) struct ChunkJob<'a> {
    pub source: &'a ChunkSource,
    pub transfer_id: Uuid,
    /// Chunk indexes the receiver does not have.
    pub missing: Vec<u64>,
    /// Requested number of data streams, clamped to 1..=[`MAX_STREAMS`].
    pub streams: usize,
    pub control: &'a TransferControl,
}

/// State shared by the workers (each in its own task) and the reply loop.
struct Shared {
    source: ChunkSource,
    transfer_id: Uuid,
    queue: async_channel::Sender<u64>,
    pending: async_channel::Receiver<u64>,
    /// One permit per chunk that may be in flight.
    window: Semaphore,
    /// Chunks sent but not answered yet, with their hashes for the cache.
    in_flight: Mutex<HashMap<u64, ChunkHash>>,
    activity: Activity,
    control: TransferControl,
}

impl Shared {
    /// Removes an answered chunk from the in-flight list and frees its window slot.
    fn answered(&self, index: u64) -> Result<ChunkHash> {
        let hash = self.in_flight.lock().ok().and_then(|mut in_flight| in_flight.remove(&index));
        let hash = hash.ok_or_else(|| Error::protocol_violation(format!("answer for chunk {index}, not in flight")))?;
        self.window.add_permits(1);
        Ok(hash)
    }
}

/// Sends every chunk in `job.missing` over parallel data streams and returns once each one is acknowledged.
///
/// Acknowledged hashes go into `cache`, progress is published on `progress`, and `acked` counts acknowledged chunks
/// (so the caller knows whether the session made progress).
pub(crate) async fn send_chunks<C, R>(
    connection: &C,
    job: ChunkJob<'_>,
    replies: &mut R,
    writer: &ControlWriter,
    cache: &mut HashCache,
    progress: &watch::Sender<Progress>,
    acked: &mut u64,
) -> Result<()>
where
    C: Connection,
    R: AsyncRead + Unpin,
{
    let streams = job.streams.clamp(1, MAX_STREAMS).min(job.missing.len());
    let (queue, pending) = async_channel::unbounded();
    for index in &job.missing {
        // An unbounded channel that is still open always accepts.
        let _ = queue.try_send(*index);
    }
    let shared = Arc::new(Shared {
        source: job.source.clone(),
        transfer_id: job.transfer_id,
        queue,
        pending,
        window: Semaphore::new(streams * IN_FLIGHT_PER_STREAM),
        in_flight: Mutex::new(HashMap::new()),
        activity: Activity::new(),
        control: job.control.clone(),
    });

    // A `JoinSet` owns the worker tasks and aborts any still running when it is dropped.
    let mut workers = JoinSet::new();
    for _ in 0..streams {
        let stream = connection.open_data(job.transfer_id).await?;
        workers.spawn(run_worker(stream, Arc::clone(&shared)));
    }
    debug!(streams, chunks = job.missing.len(), "sending chunks");

    let pause = PauseSync::new(job.control);
    let replies_done = handle_replies(replies, &shared, &pause, job.missing.len(), cache, progress, acked);
    let pause_updates = pause.run(writer);
    let watchdog = shared.activity.watchdog(job.control);
    // `pin!` lets `select!` poll the same futures again on every loop iteration.
    tokio::pin!(replies_done, pause_updates, watchdog);
    loop {
        tokio::select! {
            result = &mut replies_done => break result?,
            Some(joined) = workers.join_next() => {
                joined.map_err(|error| Error::Io(io::Error::other(error)))??;
            }
            Err(error) = &mut pause_updates => return Err(error),
            Err(error) = &mut watchdog => return Err(error),
        }
    }

    // Everything is acknowledged: wake idle workers so they close their streams, then stop the rest.
    shared.queue.close();
    shared.window.close();
    let _ = tokio::time::timeout(WORKER_SHUTDOWN_GRACE, async { while workers.join_next().await.is_some() {} }).await;
    Ok(())
}

/// One data stream: pull the next chunk, read and hash it, send its header and bytes. Stops when the queue closes.
async fn run_worker(mut stream: SendHalf, shared: Arc<Shared>) -> Result<()> {
    let mut paused = shared.control.subscribe();
    let mut buffer = Vec::new();
    loop {
        // A paused transfer starts no new chunks. `wait_for` fails only if the control handle is gone.
        if paused.wait_for(|paused| !*paused).await.is_err() {
            break;
        }
        let Ok(permit) = shared.window.acquire().await else { break };
        // The permit comes back through `Shared::answered` when the chunk's ack or nack arrives.
        permit.forget();
        let Ok(index) = shared.pending.recv().await else { break };

        let (data, hash) = shared.source.read(index, take(&mut buffer)).await?;
        if let Ok(mut in_flight) = shared.in_flight.lock() {
            in_flight.insert(index, hash);
        }
        let len = shared.source.layout().len(index);
        write_frame(&mut stream, &Message::ChunkHeader { entry: 0, index, len, hash }).await?;
        write_all_limited(&mut stream, &data, shared.control.rate_limit(), &shared.activity).await?;
        buffer = data;
    }
    debug!(transfer_id = %shared.transfer_id, "data stream finished");
    // Ends the stream cleanly; the receiver then sees the end between two chunks.
    let _ = stream.shutdown().await;
    Ok(())
}

/// Reads the receiver's answers until every chunk is acknowledged. Also handles `Pause` and `Resume`.
async fn handle_replies<R>(
    replies: &mut R,
    shared: &Shared,
    pause: &PauseSync<'_>,
    mut remaining: usize,
    cache: &mut HashCache,
    progress: &watch::Sender<Progress>,
    acked: &mut u64,
) -> Result<()>
where
    R: AsyncRead + Unpin,
{
    while remaining > 0 {
        // No timeout here: the watchdog notices silence, and allows it while paused.
        let message = read_message(replies).await?;
        shared.activity.touch();
        match message {
            Message::ChunkAck { entry: 0, index } => {
                let hash = shared.answered(index)?;
                cache.record(index, hash).await;
                remaining -= 1;
                *acked += 1;
                let len = u64::from(shared.source.layout().len(index));
                progress.send_modify(|progress| progress.transferred += len);
            }
            Message::ChunkNack { entry: 0, index } => {
                shared.answered(index)?;
                debug!(index, "receiver reported a hash mismatch, resending chunk");
                shared.queue.try_send(index).map_err(|_closed| Error::Cancelled)?;
            }
            Message::Pause => pause.apply_from_peer(true),
            Message::Resume => pause.apply_from_peer(false),
            Message::Error { code, detail } => return Err(peer_error(code, detail.as_deref())),
            other => return Err(Error::protocol_violation(format!("expected ChunkAck or ChunkNack, got {other:?}"))),
        }
    }
    Ok(())
}
