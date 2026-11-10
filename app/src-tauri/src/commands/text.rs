//! Sending short texts, and the clipboard. The clipboard is reached through these commands rather than the plugin's
//! own JavaScript API, so the window needs no clipboard permission and can only do these two things.

use tauri::{AppHandle, State};
use tauri_plugin_clipboard_manager::ClipboardExt;

use super::device_id;
use crate::{
    error::{CommandError, CommandResult},
    state::AppState,
};

/// Sends a text to a paired device and waits until that device has it.
#[tauri::command]
pub async fn send_text(state: State<'_, AppState>, peer_id: String, text: String) -> CommandResult<()> {
    let service = state.service().await;
    Ok(service.send_text(device_id(&peer_id)?, &text).await?)
}

/// The text on the clipboard, or an empty string if it holds no text.
#[tauri::command]
pub fn read_clipboard(app: AppHandle) -> String {
    // An image, a file or nothing at all is "no text", not an error the user could do anything about.
    app.clipboard().read_text().unwrap_or_default()
}

/// Puts `text` on the clipboard.
#[tauri::command]
pub fn write_clipboard(app: AppHandle, text: String) -> CommandResult<()> {
    app.clipboard().write_text(text).map_err(|error| CommandError::io(&error))
}
