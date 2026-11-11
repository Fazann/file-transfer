//! Clipboard sync with paired devices that have `sync_clipboard` on: text copied here is shared with them, and their
//! texts are put on this clipboard.
//!
//! Operating systems do not tell an app when the clipboard changes, so it is read every [`POLL_INTERVAL`], and only
//! while at least one paired device has sync on. Neither side keeps the text: only a keyed hash of the last one is
//! remembered, to tell a new copy from one already seen and to avoid sending a received text straight back.

use std::{
    hash::{BuildHasher, RandomState},
    sync::Mutex,
    time::Duration,
};

use lanshare_core::transfer::check_text;
use tauri::{AppHandle, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tokio::time::MissedTickBehavior;
use tracing::{debug, warn};

use crate::state::{lock, AppState};

/// How often the clipboard is read while sync is on for some device.
pub const POLL_INTERVAL: Duration = Duration::from_millis(750);

/// What the clipboard last held, as far as syncing is concerned.
#[derive(Default)]
pub struct ClipboardSync {
    /// Keys the hash, so the hash of a short text (a PIN, a password) cannot be looked up in a table.
    hasher: RandomState,
    /// Hash of the last text seen on or written to the clipboard. `None` until the first read after sync is turned
    /// on: whatever the clipboard holds then was copied earlier, so it is not shared.
    last: Mutex<Option<u64>>,
}

impl ClipboardSync {
    /// Records `text` as seen. Returns whether it is a new copy to share: there was an earlier read, and it differs.
    fn observe(&self, text: &str) -> bool {
        let hash = self.hasher.hash_one(text);
        lock(&self.last).replace(hash).is_some_and(|last| last != hash)
    }

    /// Forgets the last text, while sync is off for every device.
    fn reset(&self) {
        *lock(&self.last) = None;
    }

    /// Puts `text` from another device on the clipboard, recorded first so it is not shared again.
    pub fn write(&self, app: &AppHandle, text: String) {
        *lock(&self.last) = Some(self.hasher.hash_one(&text));
        if let Err(error) = app.clipboard().write_text(text) {
            warn!(%error, "could not put a synced text on the clipboard");
        }
    }
}

/// Watches the clipboard until the app exits.
pub fn watch(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            if let Some(state) = app.try_state::<AppState>() {
                poll(&app, &state).await;
            }
        }
    });
}

async fn poll(app: &AppHandle, state: &AppState) {
    // Nothing is read while no device would get it.
    if !state.trust.list().iter().any(|device| device.sync_clipboard) {
        state.clipboard.reset();
        return;
    }
    let reader = app.clone();
    // Reading the clipboard blocks, and can take a while when another app serves it.
    let Ok(Ok(text)) = tokio::task::spawn_blocking(move || reader.clipboard().read_text()).await else {
        // An image, files or an empty clipboard: nothing to share.
        return;
    };
    // Too long or empty texts are still recorded, so they are not looked at again on every read.
    if !state.clipboard.observe(&text) || check_text(&text).is_err() {
        return;
    }
    match state.service().await.share_clipboard(&text).await {
        Ok(devices) => debug!(devices, bytes = text.len(), "clipboard shared"),
        Err(error) => debug!(%error, "clipboard not shared"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_change_after_the_first_read_is_shared() {
        let sync = ClipboardSync::default();
        assert!(!sync.observe("copied before sync was on"));
        assert!(!sync.observe("copied before sync was on"));
        assert!(sync.observe("new"));
        assert!(!sync.observe("new"));
        sync.reset();
        assert!(!sync.observe("other"), "a reset starts over");
    }

    #[test]
    fn a_received_text_is_not_shared_back() {
        let sync = ClipboardSync::default();
        assert!(!sync.observe("mine"));
        // What `write` records before touching the clipboard.
        *lock(&sync.last) = Some(sync.hasher.hash_one("from the laptop"));
        assert!(!sync.observe("from the laptop"));
        assert!(sync.observe("mine again"));
    }
}
