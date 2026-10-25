//! Core library for `LanShare`: protocol, transport and file transfer logic shared by the CLI and the desktop app.
//!
//! The CLI and app are thin wrappers. Everything that touches the network or the disk lives here.

pub mod error;
pub mod protocol;
pub mod transfer;
pub mod transport;

pub use error::{Error, ErrorCode, Result};
