//! `get_self` and `list_peers`.

use serde::Serialize;
use tauri::State;

use crate::{error::CommandResult, events::PeerPayload, state::AppState};

/// This device, as other devices see it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelfPayload {
    id: String,
    name: String,
    fingerprint: String,
    port: u16,
    download_dir: String,
}

/// This device's id, name, fingerprint, port and download folder.
#[tauri::command]
pub async fn get_self(state: State<'_, AppState>) -> CommandResult<SelfPayload> {
    let service = state.service().await;
    let local = service.local();
    Ok(SelfPayload {
        id: local.id.to_string(),
        name: local.name.clone(),
        fingerprint: local.fingerprint.short(),
        port: local.port,
        download_dir: service.download_dir().display().to_string(),
    })
}

/// Every device currently found on the network.
#[tauri::command]
pub async fn list_peers(state: State<'_, AppState>) -> CommandResult<Vec<PeerPayload>> {
    let service = state.service().await;
    Ok(service.peers().iter().map(|peer| PeerPayload::new(peer, &state.trust)).collect())
}
