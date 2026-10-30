//! Tauri commands, one file per area. Every command returns `CommandResult`, so failures reach the frontend as
//! `{ code, params }`.

// Tauri hands every command argument over by value, including `State` handles.
#![allow(clippy::needless_pass_by_value)]

pub mod devices;
pub mod history;
pub mod pairing;
pub mod settings;
pub mod transfers;

use lanshare_core::{identity::DeviceId, ErrorCode};

use crate::error::{CommandError, CommandResult};

/// Parses a device id sent by the frontend. An unknown or malformed id cannot be reached.
fn device_id(text: &str) -> CommandResult<DeviceId> {
    DeviceId::parse(text).ok_or_else(|| CommandError::from(&ErrorCode::PeerUnreachable))
}
