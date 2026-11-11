//! The [`Error`] type returned by every fallible function in `lanshare-core`, and the stable [`ErrorCode`] that
//! travels on the wire and is translated by the CLI and the app.
//!
//! The `Display` text of [`Error`] is meant for logs only. User-facing text is built from [`Error::code`].

use std::io;

use serde::{Deserialize, Serialize};
use thiserror::Error as ThisError;

use crate::transport::{ConnectDiagnosis, ConnectReport};

/// Shorthand for `std::result::Result<T, lanshare_core::Error>`.
pub type Result<T> = std::result::Result<T, Error>;

/// Stable error codes, sent inside protocol messages and used as translation keys.
///
/// The variant order is part of the wire format (`postcard` encodes enum variants by index). Add new codes at the
/// end; never rename, reorder or remove existing ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    /// The receiver declined the offer.
    Declined,
    /// The two peers speak different protocol versions.
    VersionMismatch {
        /// Protocol version of the device reporting the error.
        ours: u16,
        /// Protocol version of the other device.
        theirs: u16,
    },
    /// The offered file name is empty, a directory reference, contains control characters or is too long.
    InvalidFileName,
    /// A local input/output operation failed.
    Io,
    /// The peer sent something that does not follow the protocol.
    ProtocolViolation,
    /// The connection closed or broke before the transfer finished.
    ConnectionLost,
    /// The transfer was cancelled.
    Cancelled,
    /// None of the peer's addresses accepted a connection.
    PeerUnreachable,
    /// A chunk failed its hash check three times, or the whole file failed its root hash check.
    HashMismatch,
    /// The receiving volume does not have room for the file.
    InsufficientSpace {
        /// Bytes still needed to finish the file.
        needed: u64,
        /// Bytes free on the receiving volume.
        available: u64,
    },
    /// Neither QUIC nor TCP could connect to the address.
    TransportUnavailable,
    /// The other device is not in the trust store, so the connection was refused. Pairing fixes it.
    UntrustedDevice,
    /// The certificate presented over TLS does not belong to the device that was expected.
    FingerprintMismatch,
    /// Pairing did not complete, usually because the PIN was wrong.
    PairingFailed,
    /// Too many failed pairing attempts; pairing is refused for a while.
    PairingLocked {
        /// Seconds until pairing is allowed again.
        retry_after_secs: u64,
    },
    /// The receiver is already handling as many connections as it allows.
    Busy,
    /// The operating system refused a write to the download folder.
    PermissionDenied {
        /// The folder that could not be written to, on the receiving device.
        folder: String,
    },
    /// A text is longer than a text message may be.
    TextTooLong {
        /// The most bytes of UTF-8 a text may have.
        max_bytes: u64,
    },
}

/// The name of every [`ErrorCode`], in wire order. Frontends check that each one has a translation.
pub const ERROR_CODE_NAMES: [&str; 18] = [
    "DECLINED",
    "VERSION_MISMATCH",
    "INVALID_FILE_NAME",
    "IO",
    "PROTOCOL_VIOLATION",
    "CONNECTION_LOST",
    "CANCELLED",
    "PEER_UNREACHABLE",
    "HASH_MISMATCH",
    "INSUFFICIENT_SPACE",
    "TRANSPORT_UNAVAILABLE",
    "UNTRUSTED_DEVICE",
    "FINGERPRINT_MISMATCH",
    "PAIRING_FAILED",
    "PAIRING_LOCKED",
    "BUSY",
    "PERMISSION_DENIED",
    "TEXT_TOO_LONG",
];

impl ErrorCode {
    /// Fixed `SCREAMING_SNAKE_CASE` name of the code, e.g. `"VERSION_MISMATCH"`.
    ///
    /// Frontends use it as the translation key suffix (`errors.VERSION_MISMATCH`).
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Declined => "DECLINED",
            Self::VersionMismatch { .. } => "VERSION_MISMATCH",
            Self::InvalidFileName => "INVALID_FILE_NAME",
            Self::Io => "IO",
            Self::ProtocolViolation => "PROTOCOL_VIOLATION",
            Self::ConnectionLost => "CONNECTION_LOST",
            Self::Cancelled => "CANCELLED",
            Self::PeerUnreachable => "PEER_UNREACHABLE",
            Self::HashMismatch => "HASH_MISMATCH",
            Self::InsufficientSpace { .. } => "INSUFFICIENT_SPACE",
            Self::TransportUnavailable => "TRANSPORT_UNAVAILABLE",
            Self::UntrustedDevice => "UNTRUSTED_DEVICE",
            Self::FingerprintMismatch => "FINGERPRINT_MISMATCH",
            Self::PairingFailed => "PAIRING_FAILED",
            Self::PairingLocked { .. } => "PAIRING_LOCKED",
            Self::Busy => "BUSY",
            Self::PermissionDenied { .. } => "PERMISSION_DENIED",
            Self::TextTooLong { .. } => "TEXT_TOO_LONG",
        }
    }
}

/// Errors returned by `lanshare-core`.
///
/// `thiserror` generates the `Display` and `std::error::Error` impls from the `#[error(...)]` attributes, similar to
/// writing an `Error() string` method by hand in Go.
#[derive(Debug, ThisError)]
pub enum Error {
    /// The receiver declined the offer.
    #[error("DECLINED")]
    Declined,
    /// The peer speaks a different protocol version.
    #[error("VERSION_MISMATCH (ours {ours}, theirs {theirs})")]
    VersionMismatch {
        /// Protocol version of this device.
        ours: u16,
        /// Protocol version of the peer.
        theirs: u16,
    },
    /// A file name failed sanitizing.
    #[error("INVALID_FILE_NAME")]
    InvalidFileName,
    /// A local input/output operation failed.
    #[error("IO: {0}")]
    Io(#[source] io::Error),
    /// The peer broke the protocol. `detail` is for logs only.
    #[error("PROTOCOL_VIOLATION: {detail}")]
    ProtocolViolation {
        /// What went wrong, for logs.
        detail: String,
    },
    /// The connection closed or broke before the transfer finished.
    #[error("CONNECTION_LOST")]
    ConnectionLost,
    /// The transfer was cancelled.
    #[error("CANCELLED")]
    Cancelled,
    /// None of the peer's addresses accepted a connection. The report is empty when the peer was not found at all.
    #[error("PEER_UNREACHABLE ({report})")]
    PeerUnreachable {
        /// Every attempt and why it failed.
        report: ConnectReport,
    },
    /// Received data did not match its hash.
    #[error("HASH_MISMATCH")]
    HashMismatch,
    /// The receiving volume does not have room for the file.
    #[error("INSUFFICIENT_SPACE (needed {needed}, available {available})")]
    InsufficientSpace {
        /// Bytes still needed to finish the file.
        needed: u64,
        /// Bytes free on the receiving volume.
        available: u64,
    },
    /// Neither QUIC nor TCP could connect to the address.
    #[error("TRANSPORT_UNAVAILABLE ({report})")]
    TransportUnavailable {
        /// Every attempt and why it failed.
        report: ConnectReport,
    },
    /// The other device is not trusted.
    #[error("UNTRUSTED_DEVICE")]
    UntrustedDevice,
    /// The TLS certificate does not belong to the expected device.
    #[error("FINGERPRINT_MISMATCH")]
    FingerprintMismatch,
    /// Pairing did not complete.
    #[error("PAIRING_FAILED")]
    PairingFailed,
    /// Pairing is locked after too many failures.
    #[error("PAIRING_LOCKED (retry after {retry_after_secs} s)")]
    PairingLocked {
        /// Seconds until pairing is allowed again.
        retry_after_secs: u64,
    },
    /// The receiver has no room for another connection.
    #[error("BUSY")]
    Busy,
    /// The operating system refused a write to the download folder.
    #[error("PERMISSION_DENIED ({folder})")]
    PermissionDenied {
        /// The folder that could not be written to.
        folder: String,
    },
    /// A text is longer than a text message may be.
    #[error("TEXT_TOO_LONG (at most {max_bytes} bytes)")]
    TextTooLong {
        /// The most bytes of UTF-8 a text may have.
        max_bytes: u64,
    },
    /// The peer reported an error that has no more specific local variant.
    #[error("PEER: {}", code.name())]
    Peer {
        /// The code the peer sent.
        code: ErrorCode,
    },
}

impl Error {
    /// The stable code for this error.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Declined => ErrorCode::Declined,
            Self::VersionMismatch { ours, theirs } => ErrorCode::VersionMismatch { ours: *ours, theirs: *theirs },
            Self::InvalidFileName => ErrorCode::InvalidFileName,
            Self::Io(_) => ErrorCode::Io,
            Self::ProtocolViolation { .. } => ErrorCode::ProtocolViolation,
            Self::ConnectionLost => ErrorCode::ConnectionLost,
            Self::Cancelled => ErrorCode::Cancelled,
            Self::PeerUnreachable { .. } => ErrorCode::PeerUnreachable,
            Self::HashMismatch => ErrorCode::HashMismatch,
            Self::InsufficientSpace { needed, available } => {
                ErrorCode::InsufficientSpace { needed: *needed, available: *available }
            }
            Self::TransportUnavailable { .. } => ErrorCode::TransportUnavailable,
            Self::UntrustedDevice => ErrorCode::UntrustedDevice,
            Self::FingerprintMismatch => ErrorCode::FingerprintMismatch,
            Self::PairingFailed => ErrorCode::PairingFailed,
            Self::PairingLocked { retry_after_secs } => {
                ErrorCode::PairingLocked { retry_after_secs: *retry_after_secs }
            }
            Self::Busy => ErrorCode::Busy,
            Self::PermissionDenied { folder } => ErrorCode::PermissionDenied { folder: folder.clone() },
            Self::TextTooLong { max_bytes } => ErrorCode::TextTooLong { max_bytes: *max_bytes },
            Self::Peer { code } => code.clone(),
        }
    }

    /// [`Error::PeerUnreachable`] for a device that is not found, so nothing was attempted.
    #[must_use]
    pub fn peer_not_found() -> Self {
        Self::PeerUnreachable { report: ConnectReport::default() }
    }

    /// Why a connection failed and what the user can do, for [`Error::PeerUnreachable`] and
    /// [`Error::TransportUnavailable`] after at least one attempt.
    #[must_use]
    pub fn connect_diagnosis(&self) -> Option<ConnectDiagnosis> {
        match self {
            Self::PeerUnreachable { report } | Self::TransportUnavailable { report } => report.diagnose(),
            _ => None,
        }
    }

    /// Builds the local error for a code the peer sent in `Reject` or `Error`.
    ///
    /// The peer's `VersionMismatch` is written from its point of view, so `ours` and `theirs` are swapped.
    pub(crate) fn from_peer(code: ErrorCode) -> Self {
        match code {
            ErrorCode::Declined => Self::Declined,
            ErrorCode::VersionMismatch { ours, theirs } => Self::VersionMismatch { ours: theirs, theirs: ours },
            ErrorCode::HashMismatch => Self::HashMismatch,
            ErrorCode::InsufficientSpace { needed, available } => Self::InsufficientSpace { needed, available },
            ErrorCode::UntrustedDevice => Self::UntrustedDevice,
            ErrorCode::FingerprintMismatch => Self::FingerprintMismatch,
            ErrorCode::PairingFailed => Self::PairingFailed,
            ErrorCode::PairingLocked { retry_after_secs } => Self::PairingLocked { retry_after_secs },
            ErrorCode::Busy => Self::Busy,
            ErrorCode::PermissionDenied { folder } => Self::PermissionDenied { folder },
            ErrorCode::TextTooLong { max_bytes } => Self::TextTooLong { max_bytes },
            code => Self::Peer { code },
        }
    }

    /// Shorthand for [`Error::ProtocolViolation`].
    pub(crate) fn protocol_violation(detail: impl Into<String>) -> Self {
        Self::ProtocolViolation { detail: detail.into() }
    }
}

/// Lets `?` turn an `io::Error` into an [`Error`]. Errors that mean "the other side went away" become
/// [`Error::ConnectionLost`]; everything else stays [`Error::Io`].
impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected => {
                tracing::debug!(%error, "treating io error as a lost connection");
                Self::ConnectionLost
            }
            _ => Self::Io(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_CODES: [ErrorCode; 18] = [
        ErrorCode::Declined,
        ErrorCode::VersionMismatch { ours: 1, theirs: 2 },
        ErrorCode::InvalidFileName,
        ErrorCode::Io,
        ErrorCode::ProtocolViolation,
        ErrorCode::ConnectionLost,
        ErrorCode::Cancelled,
        ErrorCode::PeerUnreachable,
        ErrorCode::HashMismatch,
        ErrorCode::InsufficientSpace { needed: 2, available: 1 },
        ErrorCode::TransportUnavailable,
        ErrorCode::UntrustedDevice,
        ErrorCode::FingerprintMismatch,
        ErrorCode::PairingFailed,
        ErrorCode::PairingLocked { retry_after_secs: 300 },
        ErrorCode::Busy,
        ErrorCode::PermissionDenied { folder: String::new() },
        ErrorCode::TextTooLong { max_bytes: 65_536 },
    ];

    #[test]
    fn name_list_matches_every_code_in_order() {
        let names: Vec<&str> = ALL_CODES.iter().map(ErrorCode::name).collect();
        assert_eq!(names, ERROR_CODE_NAMES);
    }

    #[test]
    fn names_are_unique_screaming_snake_case() {
        let mut names: Vec<&str> = ALL_CODES.iter().map(ErrorCode::name).collect();
        for name in &names {
            assert!(name.chars().all(|c| c.is_ascii_uppercase() || c == '_'), "bad name {name}");
        }
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ALL_CODES.len());
    }

    #[test]
    fn disconnect_io_errors_become_connection_lost() {
        let error = Error::from(io::Error::from(io::ErrorKind::UnexpectedEof));
        assert_eq!(error.code(), ErrorCode::ConnectionLost);
        let error = Error::from(io::Error::from(io::ErrorKind::PermissionDenied));
        assert_eq!(error.code(), ErrorCode::Io);
    }

    #[test]
    fn peer_version_mismatch_is_swapped() {
        let error = Error::from_peer(ErrorCode::VersionMismatch { ours: 1, theirs: 9 });
        assert_eq!(error.code(), ErrorCode::VersionMismatch { ours: 9, theirs: 1 });
        assert_eq!(Error::from_peer(ErrorCode::Declined).code(), ErrorCode::Declined);
        assert_eq!(Error::from_peer(ErrorCode::Io).code(), ErrorCode::Io);
    }
}
