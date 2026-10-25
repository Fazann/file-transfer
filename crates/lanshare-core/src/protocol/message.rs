//! Protocol messages exchanged between sender and receiver.

use serde::{Deserialize, Serialize};

use crate::error::ErrorCode;

/// One framed protocol message.
///
/// The variant order is part of the wire format. `Hello` must stay the first variant with the same fields forever,
/// so peers on any version can decode it and report a version mismatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    /// First message from the sender.
    Hello {
        /// Human-readable name of the sending device.
        device_name: String,
        /// The sender's [`PROTOCOL_VERSION`](super::PROTOCOL_VERSION).
        protocol_version: u16,
    },
    /// The sender offers one file.
    Offer {
        /// File name only, without directories. The receiver sanitizes it again.
        file_name: String,
        /// Exact file size in bytes.
        size: u64,
    },
    /// The receiver accepts the offer. The sender then streams `size` raw, unframed bytes.
    Accept,
    /// The receiver refuses the offer or the hello.
    Reject {
        /// Why the receiver refused.
        code: ErrorCode,
    },
    /// The sender has sent every byte.
    Done,
    /// Either side reports a failure.
    Error {
        /// What went wrong.
        code: ErrorCode,
        /// Extra detail for logs only, never shown in the UI.
        detail: Option<String>,
    },
}
