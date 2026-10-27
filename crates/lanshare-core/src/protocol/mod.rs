//! The `LanShare` wire protocol: message types and length-prefixed framing. See `PROTOCOL.md` at the repo root.

mod frame;
mod message;

pub(crate) use frame::{decode_frame, read_raw_frame};
pub use frame::{read_message, read_message_or_end, write_message, MAX_FRAME_SIZE};
pub use message::{EntryResume, Message, OfferEntry};

/// Version of the wire format. Increase by 1 on every wire change and record it in `PROTOCOL.md`.
pub const PROTOCOL_VERSION: u16 = 4;
