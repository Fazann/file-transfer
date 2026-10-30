//! Core library for `LanShare`: protocol, transport and file transfer logic shared by the CLI and the desktop app.
//!
//! The CLI and app are thin wrappers. Everything that touches the network or the disk lives here.

pub mod config;
pub mod discovery;
pub mod error;
pub mod identity;
pub mod pairing;
pub mod paths;
pub mod protocol;
pub mod service;
pub mod transfer;
pub mod transport;

pub use error::{Error, ErrorCode, Result, ERROR_CODE_NAMES};
