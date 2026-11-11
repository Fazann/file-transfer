//! Sending and receiving files in hashed chunks over parallel streams, with resume after a dropped connection,
//! pause and a bandwidth limit; and short texts in a single message.
//!
//! Sender and receiver work on any [`Connection`](crate::transport::Connection), so the same code runs over QUIC and
//! TCP.

mod bench;
mod chunk;
mod control;
mod hash_cache;
mod manifest;
mod path;
mod progress;
mod rate_limit;
mod receiver;
mod retry;
mod scheduler;
mod sender;
mod state;
mod text;
mod writer;

use std::{
    fs,
    future::Future,
    io,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

pub use bench::{run_bench, BenchResult};
pub use chunk::{
    chunk_count, hash_chunk, root_hash, ChunkHash, ChunkLayout, DEFAULT_CHUNK_SIZE, DEFAULT_CHUNK_SIZE_MIB,
    MAX_CHUNK_COUNT, MAX_CHUNK_SIZE, MAX_CHUNK_SIZE_MIB,
};
pub(crate) use control::ControlWriter;
pub use control::TransferControl;
pub use hash_cache::HASH_CACHE_MAX_AGE;
pub use manifest::{list_entries, MAX_ENTRIES};
pub use path::{sanitize_file_name, sanitize_relative_path, RelativePath};
pub use progress::{progress_channel, Progress};
pub use rate_limit::RateLimit;
pub use receiver::{receive_file, IncomingOffer, OfferDecider, ReceiveOutcome, ReceivePolicy, ReceivedFile};
pub use retry::RETRY_WINDOW;
pub use scheduler::{DEFAULT_STREAMS, MAX_STREAMS};
pub use sender::{send_file, send_files, SendOptions};
pub use state::{cleanup_stale_parts, PART_SUFFIX, STATE_SUFFIX};
pub use text::{check_text, send_text, ReceivedText, MAX_TEXT_BYTES};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::timeout,
};

use self::control::Activity;
use crate::{
    error::{Error, Result},
    protocol::{read_message, write_message, Message},
};

/// If a connection moves no data for this long during a transfer, it counts as lost.
///
/// TCP alone can take many minutes to notice that Wi-Fi went away. The receiver must notice well within the sender's
/// [`RETRY_WINDOW`], because it handles one connection at a time and the sender's reconnect waits behind it.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(20);

/// Largest slice of chunk data moved in one read or write (256 KiB). The idle timeout and the bandwidth limit work
/// per slice, so they measure stalls and rates, not chunk sizes.
const BUFFER_SIZE: usize = 256 * 1024;

/// Resume state and hash cache are saved after this many newly completed chunks.
const SAVE_EVERY_CHUNKS: u32 = 8;

/// Runs `operation`, failing with [`Error::ConnectionLost`] if it takes longer than [`IDLE_TIMEOUT`].
async fn idle<T>(operation: impl Future<Output = Result<T>>) -> Result<T> {
    timeout(IDLE_TIMEOUT, operation).await.map_err(|_elapsed| Error::ConnectionLost)?
}

/// [`read_message`] with the idle timeout.
async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Message> {
    idle(read_message(reader)).await
}

/// [`write_message`] with the idle timeout.
async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, message: &Message) -> Result<()> {
    idle(write_message(writer, message)).await
}

/// Fills `buffer` from `reader` one slice at a time, each with the idle timeout, recording activity as data arrives.
async fn read_exact_tracked<R: AsyncRead + Unpin>(
    reader: &mut R,
    buffer: &mut [u8],
    activity: &Activity,
) -> Result<()> {
    for slice in buffer.chunks_mut(BUFFER_SIZE) {
        // `read_exact` returns the byte count, which is always the slice length; `?` maps EOF to `ConnectionLost`.
        idle(async { Ok(reader.read_exact(slice).await?) }).await?;
        activity.touch();
    }
    Ok(())
}

/// Writes all of `data` one slice at a time: waits for the bandwidth limit, writes with the idle timeout, and
/// records activity.
async fn write_all_limited<W: AsyncWrite + Unpin>(
    writer: &mut W,
    data: &[u8],
    rate_limit: &RateLimit,
    activity: &Activity,
) -> Result<()> {
    for slice in data.chunks(BUFFER_SIZE) {
        rate_limit.wait_for(slice.len()).await;
        idle(async { Ok(writer.write_all(slice).await?) }).await?;
        activity.touch();
    }
    idle(async { Ok(writer.flush().await?) }).await
}

/// Turns an `Error` message from the peer into a local error, logging its detail.
fn peer_error(code: crate::ErrorCode, detail: Option<&str>) -> Error {
    tracing::warn!(code = code.name(), ?detail, "the other device reported an error");
    Error::from_peer(code)
}

/// Writes `contents` to a temporary file next to `path` and renames it over `path`, so readers never see a
/// half-written file. Blocking.
///
/// Each call uses its own temporary name (`<path>.<n>.tmp`). A cancelled save may still be running on the blocking
/// pool when the next one starts, and the two must not write into the same temporary file.
fn write_file_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)));
    fs::write(&temporary, contents)?;
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use tokio::io::duplex;

    use super::*;

    #[tokio::test]
    async fn large_buffers_move_in_slices() {
        let (mut client, mut server) = duplex(64 * 1024);
        let data: Vec<u8> = (0..3 * BUFFER_SIZE + 5).map(|i| u8::try_from(i % 251).unwrap()).collect();
        let expected = data.clone();
        let writer = tokio::spawn(async move {
            write_all_limited(&mut client, &data, &RateLimit::default(), &Activity::new()).await
        });
        let mut received = vec![0; expected.len()];
        read_exact_tracked(&mut server, &mut received, &Activity::new()).await.unwrap();
        writer.await.unwrap().unwrap();
        assert_eq!(received, expected);
    }

    #[tokio::test(start_paused = true)]
    async fn silent_peer_times_out_as_connection_lost() {
        // With a paused clock, tokio skips ahead to the timeout instead of really waiting 20 seconds.
        let (_client, mut server) = duplex(64);
        let error = read_frame(&mut server).await.unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::ConnectionLost);
    }
}
