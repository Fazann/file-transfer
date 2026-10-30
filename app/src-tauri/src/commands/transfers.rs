//! Sending, answering offers, and pausing, resuming or cancelling transfers.

use std::path::PathBuf;

use lanshare_core::service::TransferId;
use tauri::State;

use super::device_id;
use crate::{error::CommandResult, state::AppState};

/// Sends files and folders to a device as one transfer. Returns its id.
#[tauri::command]
pub async fn send_files(state: State<'_, AppState>, peer_id: String, paths: Vec<String>) -> CommandResult<TransferId> {
    let paths = paths.into_iter().map(PathBuf::from).collect();
    Ok(state.service().await.send(device_id(&peer_id)?, paths)?)
}

/// Accepts or declines an incoming offer. `false` if it is no longer waiting.
#[tauri::command]
pub async fn respond_to_offer(state: State<'_, AppState>, request_id: u64, accept: bool) -> CommandResult<bool> {
    Ok(state.service().await.respond_to_offer(request_id, accept))
}

/// Pauses a transfer. `false` for an unknown id.
#[tauri::command]
pub async fn pause(state: State<'_, AppState>, id: TransferId) -> CommandResult<bool> {
    Ok(state.service().await.pause(id))
}

/// Resumes a transfer. `false` for an unknown id.
#[tauri::command]
pub async fn resume(state: State<'_, AppState>, id: TransferId) -> CommandResult<bool> {
    Ok(state.service().await.resume(id))
}

/// Cancels a transfer. `false` for an unknown id.
#[tauri::command]
pub async fn cancel(state: State<'_, AppState>, id: TransferId) -> CommandResult<bool> {
    Ok(state.service().await.cancel(id))
}

/// Pauses every transfer.
#[tauri::command]
pub async fn pause_all(state: State<'_, AppState>) -> CommandResult<()> {
    state.service().await.pause_all();
    Ok(())
}
