//! Throughput benchmark (`lanshare bench`): the sender streams generated data over parallel data streams and the
//! receiver throws it away, so neither disk nor hashing is measured, only the network and the transport.

use std::{
    convert::Infallible,
    io,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncRead, AsyncWriteExt},
    sync::mpsc,
    task::JoinSet,
};
use tracing::info;
use uuid::Uuid;

use super::{
    chunk::{ChunkLayout, DEFAULT_CHUNK_SIZE},
    control::{Activity, ControlWriter},
    peer_error,
    rate_limit::RateLimit,
    read_exact_tracked, read_frame,
    scheduler::MAX_STREAMS,
    write_all_limited, write_frame,
};
use crate::{
    error::{Error, Result},
    identity::DeviceInfo,
    protocol::{read_message_or_end, Message, PROTOCOL_VERSION},
    transport::{Connection, RecvHalf, SendHalf},
};

/// Result of [`run_bench`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BenchResult {
    /// Bytes sent.
    pub bytes: u64,
    /// From the receiver's `Accept` until its `Done` (all bytes arrived).
    pub elapsed: Duration,
    /// Data streams used.
    pub streams: usize,
}

impl BenchResult {
    /// Throughput in MB/s (1 MB = 1,000,000 bytes).
    #[must_use]
    pub fn megabytes_per_second(&self) -> f64 {
        // Precision loss in `as f64` only matters above 2^53 bytes, far beyond any benchmark size.
        #[allow(clippy::cast_precision_loss)]
        let bytes = self.bytes as f64;
        bytes / 1_000_000.0 / self.elapsed.as_secs_f64().max(f64::EPSILON)
    }
}

/// Sends `size` bytes of generated data over `streams` data streams and measures how long it takes.
///
/// # Errors
///
/// - [`Error::VersionMismatch`] if the receiver refuses the hello
/// - [`Error::ConnectionLost`] if the connection drops
/// - [`Error::ProtocolViolation`] for an unexpected reply or a `size` too large for the chunk layout
pub async fn run_bench<C: Connection>(
    connection: &C,
    local: &DeviceInfo,
    size: u64,
    streams: usize,
) -> Result<BenchResult> {
    let layout = ChunkLayout::new(size, DEFAULT_CHUNK_SIZE)?;
    let (send, mut recv) = connection.open_control().await?;
    let writer = ControlWriter::new(send);
    let hello =
        Message::Hello { protocol_version: PROTOCOL_VERSION, device_id: local.id, device_name: local.name.clone() };
    writer.send(&hello).await?;
    // A fresh id, so the TCP listener can tell this benchmark's data connections from any other transfer's.
    let transfer_id = Uuid::new_v4();
    writer.send(&Message::BenchOffer { transfer_id, size }).await?;
    match read_frame(&mut recv).await? {
        Message::Accept { .. } => {}
        Message::Reject { code } => return Err(Error::from_peer(code)),
        other => return Err(Error::protocol_violation(format!("expected Accept, got {other:?}"))),
    }

    let start = Instant::now();
    let streams = streams.clamp(1, MAX_STREAMS);
    let (queue, pending) = async_channel::unbounded();
    for index in 0..layout.count() {
        let _ = queue.try_send(index);
    }
    queue.close();
    // One buffer of non-repeating bytes, shared by every stream, so compression on the path cannot flatter results.
    let pattern: Arc<Vec<u8>> =
        Arc::new((0..DEFAULT_CHUNK_SIZE).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect());
    let mut workers = JoinSet::new();
    for _ in 0..streams {
        let stream = connection.open_data(transfer_id).await?;
        workers.spawn(send_stream(stream, layout, pending.clone(), Arc::clone(&pattern)));
    }
    while let Some(joined) = workers.join_next().await {
        joined.map_err(|error| Error::Io(io::Error::other(error)))??;
    }
    match read_frame(&mut recv).await? {
        Message::Done => {}
        Message::Error { code, detail } => return Err(peer_error(code, detail.as_deref())),
        other => return Err(Error::protocol_violation(format!("expected Done, got {other:?}"))),
    }
    let elapsed = start.elapsed();
    writer.shutdown().await;
    Ok(BenchResult { bytes: size, elapsed, streams })
}

async fn send_stream(
    mut stream: SendHalf,
    layout: ChunkLayout,
    pending: async_channel::Receiver<u64>,
    pattern: Arc<Vec<u8>>,
) -> Result<()> {
    let activity = Activity::new();
    let unlimited = RateLimit::default();
    while let Ok(index) = pending.recv().await {
        let len = layout.len(index);
        // The hash is not checked in a benchmark.
        write_frame(&mut stream, &Message::ChunkHeader { entry: 0, index, len, hash: [0; 32] }).await?;
        write_all_limited(&mut stream, &pattern[..len as usize], &unlimited, &activity).await?;
    }
    let _ = stream.shutdown().await;
    Ok(())
}

/// Receiver side of a benchmark: accepts it, discards `size` bytes from the data streams, then answers `Done`.
/// Returns the bytes received.
pub(crate) async fn receive_bench<C, R>(
    connection: &C,
    recv: &mut R,
    writer: &ControlWriter,
    transfer_id: Uuid,
    size: u64,
) -> Result<u64>
where
    C: Connection,
    R: AsyncRead + Unpin,
{
    info!(size, "benchmark started");
    writer.send(&Message::Accept { resume: Vec::new() }).await?;
    let (counts, mut counted) = mpsc::unbounded_channel();
    let total = async {
        let mut received = 0;
        while received < size {
            received += counted.recv().await.ok_or(Error::ConnectionLost)?;
        }
        Ok::<_, Error>(received)
    };
    // `recv` is not read during the benchmark; the sender says nothing on the control channel until it is done.
    let received = tokio::select! {
        Err(error) = accept_bench_streams(connection, transfer_id, counts) => return Err(error),
        received = total => received?,
    };
    let _ = recv;
    writer.send(&Message::Done).await?;
    Ok(received)
}

/// Accepts the benchmark's data streams and starts a discarding reader for each. Only returns on an error.
async fn accept_bench_streams<C: Connection>(
    connection: &C,
    transfer_id: Uuid,
    counts: mpsc::UnboundedSender<u64>,
) -> Result<Infallible> {
    let mut readers = JoinSet::new();
    loop {
        tokio::select! {
            stream = connection.accept_data(transfer_id) => {
                if readers.len() >= MAX_STREAMS {
                    return Err(Error::protocol_violation("too many data streams"));
                }
                readers.spawn(discard_stream(stream?, counts.clone()));
            }
            Some(joined) = readers.join_next() => {
                // A reader only ever ends with an error; `match` on `Infallible` has no arms to write.
                match joined.map_err(|error| Error::Io(io::Error::other(error)))?? {}
            }
        }
    }
}

async fn discard_stream(mut stream: RecvHalf, counts: mpsc::UnboundedSender<u64>) -> Result<Infallible> {
    let activity = Activity::new();
    let mut buffer = vec![0; DEFAULT_CHUNK_SIZE as usize];
    while let Some(message) = read_message_or_end(&mut stream).await? {
        let Message::ChunkHeader { len, .. } = message else {
            return Err(Error::protocol_violation(format!("unexpected {message:?} on a data stream")));
        };
        let len = (len as usize).min(buffer.len());
        read_exact_tracked(&mut stream, &mut buffer[..len], &activity).await?;
        let _ = counts.send(len as u64);
    }
    // The stream ended; wait until the benchmark finishes instead of ending the accept loop.
    std::future::pending().await
}
