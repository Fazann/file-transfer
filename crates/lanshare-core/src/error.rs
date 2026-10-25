//! The [`Error`] type returned by every fallible function in `lanshare-core`, and the stable [`ErrorCode`] that
//! travels on the wire and is translated by the CLI and the app.
//!
//! The `Display` text of [`Error`] is meant for logs only. User-facing text is built from [`Error::code`].

use std::io;

use serde::{Deserialize, Serialize};
use thiserror::Error as ThisError;

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
}

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
    /// None of the peer's addresses accepted a connection.
    #[error("PEER_UNREACHABLE")]
    PeerUnreachable,
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
            Self::PeerUnreachable => ErrorCode::PeerUnreachable,
            Self::Peer { code } => code.clone(),
        }
    }

    /// Builds the local error for a code the peer sent in `Reject` or `Error`.
    ///
    /// The peer's `VersionMismatch` is written from its point of view, so `ours` and `theirs` are swapped.
    pub(crate) fn from_peer(code: ErrorCode) -> Self {
        match code {
            ErrorCode::Declined => Self::Declined,
            ErrorCode::VersionMismatch { ours, theirs } => Self::VersionMismatch { ours: theirs, theirs: ours },
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
            | io::ErrorKind::BrokenPipe => {
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

    const ALL_CODES: [ErrorCode; 8] = [
        ErrorCode::Declined,
        ErrorCode::VersionMismatch { ours: 1, theirs: 2 },
        ErrorCode::InvalidFileName,
        ErrorCode::Io,
        ErrorCode::ProtocolViolation,
        ErrorCode::ConnectionLost,
        ErrorCode::Cancelled,
        ErrorCode::PeerUnreachable,
    ];

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
