//! The `LanShare` wire protocol: message types and length-prefixed framing. See `PROTOCOL.md` at the repo root.

mod frame;
mod message;

pub(crate) use frame::{decode_frame, read_raw_frame};
pub use frame::{read_message, read_message_or_end, write_message, MAX_FRAME_SIZE};
pub use message::{EntryResume, Message, OfferEntry};

/// Version of the wire format, frozen for the 1.x releases: new versions must stay compatible with it, or increase
/// this by 1 deliberately and record the change in `PROTOCOL.md`.
pub const PROTOCOL_VERSION: u16 = 6;
