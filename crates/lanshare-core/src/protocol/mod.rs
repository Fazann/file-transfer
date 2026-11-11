//! The `LanShare` wire protocol: message types and length-prefixed framing. See `PROTOCOL.md` at the repo root.

mod frame;
mod message;

pub(crate) use frame::{decode_frame, read_raw_frame};
pub use frame::{read_message, read_message_or_end, write_message, MAX_FRAME_SIZE};
pub use message::{EntryResume, Message, OfferEntry};

/// The newest version of the wire format this build speaks. Version 7 adds `TextOffer` to version 6, and version 8 adds
/// `ClipboardOffer`; neither changes anything else, so all three are accepted (see `PROTOCOL.md`). Increase this by 1
/// deliberately and record the change there.
pub const PROTOCOL_VERSION: u16 = 8;

/// The version that added `TextOffer`, announced before one.
pub const TEXT_PROTOCOL_VERSION: u16 = 7;

/// The oldest version a receiver accepts: version 6, frozen for the 1.x releases.
///
/// A sender announces the lowest version that has every message it sends: [`PROTOCOL_VERSION`] before a
/// `ClipboardOffer`, [`TEXT_PROTOCOL_VERSION`] before a `TextOffer`, this for everything else, so devices running 1.0
/// still accept its files.
pub const MIN_PROTOCOL_VERSION: u16 = 6;

/// Whether a peer's `Hello` version can be talked to.
#[must_use]
pub const fn is_supported_version(version: u16) -> bool {
    version >= MIN_PROTOCOL_VERSION && version <= PROTOCOL_VERSION
}
