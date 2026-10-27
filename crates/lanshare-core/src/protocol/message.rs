//! Protocol messages exchanged between sender and receiver.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{error::ErrorCode, identity::DeviceId};

/// One file in an [`Message::Offer`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferEntry {
    /// File name only until phase 7, which adds relative paths for folders. The receiver sanitizes it again.
    pub relative_path: String,
    /// Exact file size in bytes.
    pub size: u64,
}

/// Which chunks of one entry the receiver already has, sent in [`Message::Accept`] when resuming.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryResume {
    /// Index of the entry in the offer.
    pub entry: u32,
    /// Bitset of received chunks: chunk `i` is bit `i % 8` (least significant first) of byte `i / 8`.
    pub have: Vec<u8>,
}

/// One framed protocol message.
///
/// The variant order is part of the wire format. `Hello` must stay the first variant and `protocol_version` must stay
/// its first field forever, so any version can read the peer's version and report a mismatch (see `frame.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    /// First message from the sender.
    Hello {
        /// The sender's [`PROTOCOL_VERSION`](super::PROTOCOL_VERSION). Always the first field.
        protocol_version: u16,
        /// Stable id of the sending device.
        device_id: DeviceId,
        /// Human-readable name of the sending device.
        device_name: String,
    },
    /// The sender offers files. Exactly one entry until phase 7. Sent on the control stream, like every message
    /// except the chunk data.
    Offer {
        /// Stays the same when the same file is offered again, so the receiver can resume.
        transfer_id: Uuid,
        /// Size of every chunk in bytes, except the last chunk of each entry which may be shorter.
        chunk_size: u32,
        /// The offered files.
        entries: Vec<OfferEntry>,
    },
    /// The receiver accepts the offer. The sender then sends the chunks the receiver does not have.
    Accept {
        /// Chunks already received, per entry. Empty for a new transfer.
        resume: Vec<EntryResume>,
    },
    /// The receiver refuses the offer or the hello.
    Reject {
        /// Why the receiver refused.
        code: ErrorCode,
    },
    /// One chunk of file data follows this frame as exactly `len` raw, unframed bytes. Sent on data streams only.
    ChunkHeader {
        /// Index of the entry in the offer.
        entry: u32,
        /// Index of the chunk in the entry.
        index: u64,
        /// Number of raw bytes after this frame.
        len: u32,
        /// BLAKE3 hash of the raw bytes.
        hash: [u8; 32],
    },
    /// The receiver verified and wrote a chunk.
    ChunkAck {
        /// Index of the entry in the offer.
        entry: u32,
        /// Index of the chunk in the entry.
        index: u64,
    },
    /// The chunk's bytes did not match its hash. The sender sends it again.
    ChunkNack {
        /// Index of the entry in the offer.
        entry: u32,
        /// Index of the chunk in the entry.
        index: u64,
    },
    /// Every chunk of an entry was acknowledged.
    EntryDone {
        /// Index of the entry in the offer.
        entry: u32,
        /// BLAKE3 of all chunk hashes of the entry, concatenated in index order.
        root_hash: [u8; 32],
    },
    /// Sent by the sender after the last `EntryDone`, and answered by the receiver once every entry is saved.
    Done,
    /// Either side reports a failure.
    Error {
        /// What went wrong.
        code: ErrorCode,
        /// Extra detail for logs only, never shown in the UI.
        detail: Option<String>,
    },
    /// First frame of every extra TCP connection, marking it as a data stream of this transfer. Not used over QUIC.
    DataStreamHello {
        /// The `transfer_id` from the offer.
        transfer_id: Uuid,
    },
    /// Either side asks to pause: the sender stops starting new chunks; chunks already on their way finish.
    Pause,
    /// Either side asks to continue after `Pause`.
    Resume,
    /// Sent instead of `Offer` to measure throughput: `size` bytes of generated data that the receiver discards.
    BenchOffer {
        /// Random id of this benchmark, used like a `transfer_id` for `DataStreamHello`.
        transfer_id: Uuid,
        /// Bytes the sender will send.
        size: u64,
    },
}
