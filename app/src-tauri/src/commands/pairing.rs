//! Pairing and the trusted device list.

use lanshare_core::ErrorCode;
use tauri::State;

use super::device_id;
use crate::{
    error::{CommandError, CommandResult},
    events::TrustedPayload,
    state::AppState,
};

/// Starts pairing with a device; it shows a PIN. Returns the id to pass to `submit_pin`.
#[tauri::command]
pub async fn start_pairing(state: State<'_, AppState>, peer_id: String) -> CommandResult<u64> {
    Ok(state.service().await.start_pairing(device_id(&peer_id)?)?)
}

/// Gives the PIN the user typed, or `null` to cancel. `false` if that pairing is not waiting for a PIN.
#[tauri::command]
pub async fn submit_pin(state: State<'_, AppState>, request_id: u64, pin: Option<String>) -> CommandResult<bool> {
    Ok(state.service().await.submit_pin(request_id, pin))
}

/// Paired devices, re-read from disk in case the CLI changed them.
#[tauri::command]
pub async fn list_trusted(state: State<'_, AppState>) -> CommandResult<Vec<TrustedPayload>> {
    state.trust.refresh().await?;
    Ok(state.trust.list().iter().map(TrustedPayload::from).collect())
}

/// Forgets a paired device.
#[tauri::command]
pub async fn remove_trusted(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    if state.trust.remove(device_id(&id)?).await? {
        Ok(())
    } else {
        Err(CommandError::from(&ErrorCode::UntrustedDevice))
    }
}

/// Turns accepting files without asking on or off for a paired device.
#[tauri::command]
pub async fn set_auto_accept(state: State<'_, AppState>, id: String, auto_accept: bool) -> CommandResult<()> {
    if state.trust.set_auto_accept(device_id(&id)?, auto_accept).await? {
        Ok(())
    } else {
        Err(CommandError::from(&ErrorCode::UntrustedDevice))
    }
}

/// Turns clipboard sync on or off for a paired device. Texts copied on either device reach the other only when both
/// have it on for each other.
#[tauri::command]
pub async fn set_sync_clipboard(state: State<'_, AppState>, id: String, sync_clipboard: bool) -> CommandResult<()> {
    if state.trust.set_sync_clipboard(device_id(&id)?, sync_clipboard).await? {
        Ok(())
    } else {
        Err(CommandError::from(&ErrorCode::UntrustedDevice))
    }
}
