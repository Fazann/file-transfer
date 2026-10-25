//! Sending and receiving a single file.
//!
//! Sender and receiver are generic over any `AsyncRead + AsyncWrite + Unpin` stream, so they work the same over TCP
//! today and QUIC streams later.

mod path;
mod progress;
mod receiver;
mod sender;

pub use path::sanitize_file_name;
pub use progress::{progress_channel, Progress};
pub use receiver::{receive_file, IncomingOffer, OfferDecider, ReceiveOutcome, ReceivedFile};
pub use sender::send_file;

/// Size of the buffer used to stream file bytes (1 MiB). This is the most file data held in memory at once.
const BUFFER_SIZE: usize = 1024 * 1024;

/// How many bytes to move in the next read: the rest of the file, but never more than [`BUFFER_SIZE`].
fn chunk_len(remaining: u64) -> usize {
    usize::try_from(remaining).map_or(BUFFER_SIZE, |remaining| remaining.min(BUFFER_SIZE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_len_is_capped_by_buffer() {
        assert_eq!(chunk_len(0), 0);
        assert_eq!(chunk_len(10), 10);
        assert_eq!(chunk_len(u64::MAX), BUFFER_SIZE);
    }
}
