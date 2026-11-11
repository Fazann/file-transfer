//! Forwards `ServiceEvent`s to the frontend as Tauri events, and records finished transfers in the history.
//!
//! Event names and payloads match `src/lib/types` in the frontend. Progress is limited to
//! [`PROGRESS_PER_SECOND`] events per transfer.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use lanshare_core::{
    discovery::{Peer, PeerEvent},
    identity::{TrustStore, TrustedDevice},
    pairing::PairingPeer,
    service::{Direction, LanShareService, ServiceEvent, TransferId, TransferInfo},
    transfer::{IncomingOffer, ReceivedText},
    ErrorCode,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{
    error::CommandError,
    history::{History, HistoryEntry, Status},
    state::AppState,
};

/// Most progress events per second for one transfer.
pub const PROGRESS_PER_SECOND: u32 = 10;

/// A device on the network.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerPayload {
    /// Device id.
    pub id: String,
    /// Device name.
    pub name: String,
    /// `windows`, `macos` or `linux`.
    pub os: &'static str,
    /// Short fingerprint, e.g. `A1B2-C3D4-E5F6`.
    pub fingerprint: String,
    /// Whether it is paired.
    pub trusted: bool,
}

impl PeerPayload {
    /// The payload for `peer`, marked trusted if its fingerprint is in `trust`.
    pub fn new(peer: &Peer, trust: &TrustStore) -> Self {
        Self {
            id: peer.id.to_string(),
            name: peer.name.clone(),
            os: peer.os.as_str(),
            fingerprint: peer.fingerprint.short(),
            trusted: trust.get_by_fingerprint(&peer.fingerprint).is_some(),
        }
    }
}

/// A paired device.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustedPayload {
    /// Device id.
    pub id: String,
    /// Name at pairing time.
    pub name: String,
    /// Short fingerprint.
    pub fingerprint: String,
    /// Milliseconds since 1970-01-01 UTC.
    pub paired_at: u64,
    /// Accept its files without asking.
    pub auto_accept: bool,
    /// Share the clipboard with it.
    pub sync_clipboard: bool,
}

impl From<&TrustedDevice> for TrustedPayload {
    fn from(device: &TrustedDevice) -> Self {
        Self {
            id: device.id.to_string(),
            name: device.name.clone(),
            fingerprint: device.fingerprint.short(),
            paired_at: device.paired_at.saturating_mul(1000),
            auto_accept: device.auto_accept,
            sync_clipboard: device.sync_clipboard,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OfferPayload {
    request_id: u64,
    device_id: String,
    device_name: String,
    fingerprint: String,
    trusted: bool,
    file_name: String,
    file_count: usize,
    size: u64,
}

impl OfferPayload {
    fn new(request_id: u64, offer: &IncomingOffer) -> Self {
        Self {
            request_id,
            device_id: offer.device_id.to_string(),
            device_name: offer.device_name.clone(),
            fingerprint: offer.fingerprint.short(),
            trusted: offer.trusted,
            file_name: offer.file_name.clone(),
            file_count: offer.file_count,
            size: offer.size,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferPayload {
    id: TransferId,
    direction: &'static str,
    peer_id: String,
    peer_name: String,
    file_name: String,
    file_count: usize,
    size: u64,
}

impl From<&TransferInfo> for TransferPayload {
    fn from(info: &TransferInfo) -> Self {
        Self {
            id: info.id,
            direction: direction_name(info.direction),
            peer_id: info.peer_id.to_string(),
            peer_name: info.peer_name.clone(),
            file_name: info.file_name.clone(),
            file_count: info.file_count,
            size: info.size,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressPayload {
    id: TransferId,
    transferred: u64,
    total: u64,
    paused: bool,
    /// The file being transferred, for transfers of several files.
    current_file: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CompletePayload {
    id: TransferId,
    path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FailedPayload {
    id: TransferId,
    error: CommandError,
}

/// A text from a paired device. Not `Debug`: the text may be a password and must never reach a log.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TextPayload {
    device_id: String,
    device_name: String,
    fingerprint: String,
    text: String,
}

impl From<ReceivedText> for TextPayload {
    fn from(received: ReceivedText) -> Self {
        Self {
            device_id: received.device_id.to_string(),
            device_name: received.device_name,
            fingerprint: received.fingerprint.short(),
            text: received.text,
        }
    }
}

/// Who put a text on the clipboard. The text itself stays out of the window.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClipboardPayload {
    device_name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PinPayload {
    request_id: u64,
    device_name: String,
    fingerprint: String,
    pin: String,
}

impl PinPayload {
    fn new(request_id: u64, peer: &PairingPeer, pin: String) -> Self {
        Self { request_id, device_name: peer.device_name.clone(), fingerprint: peer.fingerprint.short(), pin }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PairingResultPayload {
    request_id: u64,
    device: Option<TrustedPayload>,
    error: Option<CommandError>,
}

fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Send => "send",
        Direction::Receive => "receive",
    }
}

/// Forwards every event of `service` to the frontend until the service stops.
pub fn forward(app: AppHandle, service: LanShareService, mut events: UnboundedReceiver<ServiceEvent>) {
    tauri::async_runtime::spawn(async move {
        let mut forwarder = Forwarder { app, service, active: HashMap::new(), last_progress: HashMap::new() };
        while let Some(event) = events.recv().await {
            forwarder.handle(event);
        }
    });
}

struct Forwarder {
    app: AppHandle,
    service: LanShareService,
    /// Running transfers, to write their history entry when they end.
    active: HashMap<TransferId, TransferInfo>,
    /// When progress was last sent per transfer, for throttling.
    last_progress: HashMap<TransferId, Instant>,
}

impl Forwarder {
    fn handle(&mut self, event: ServiceEvent) {
        match event {
            ServiceEvent::Peer(PeerEvent::Found(peer)) => {
                self.emit("peer-found", PeerPayload::new(&peer, self.service.trust()));
            }
            ServiceEvent::Peer(PeerEvent::Updated(peer)) => {
                self.emit("peer-updated", PeerPayload::new(&peer, self.service.trust()));
            }
            ServiceEvent::Peer(PeerEvent::Lost(id)) => self.emit("peer-lost", id.to_string()),
            ServiceEvent::IncomingOffer { request_id, offer } => {
                self.emit("incoming-offer", OfferPayload::new(request_id, &offer));
            }
            ServiceEvent::OfferClosed { request_id } => self.emit("offer-closed", request_id),
            ServiceEvent::TransferStarted(info) => {
                self.emit("transfer-started", TransferPayload::from(&info));
                self.active.insert(info.id, info);
            }
            ServiceEvent::TransferProgress { id, progress, paused } => {
                let interval = Duration::from_secs(1) / PROGRESS_PER_SECOND;
                let now = Instant::now();
                if self.last_progress.get(&id).is_some_and(|last| now.duration_since(*last) < interval) {
                    return;
                }
                self.last_progress.insert(id, now);
                let (transferred, total) = (progress.transferred, progress.total);
                let current_file = self
                    .active
                    .get(&id)
                    .filter(|info| info.entry_names.len() > 1)
                    .and_then(|info| info.entry_names.get(progress.current_entry as usize).cloned());
                self.emit("transfer-progress", ProgressPayload { id, transferred, total, paused, current_file });
            }
            ServiceEvent::TransferComplete { id, path } => {
                let path = path.map(|path| path.display().to_string());
                self.record(id, Status::Completed, None, path.clone());
                self.emit("transfer-complete", CompletePayload { id, path });
            }
            ServiceEvent::TransferFailed { id, error, diagnosis } => {
                let status = if error == ErrorCode::Cancelled { Status::Cancelled } else { Status::Failed };
                self.record(id, status, Some(error.name().to_owned()), None);
                let error = CommandError::from(&error).with_diagnosis(diagnosis.as_ref());
                self.emit("transfer-failed", FailedPayload { id, error });
            }
            ServiceEvent::ReceiveRefused { error } => self.emit("receive-refused", CommandError::from(&error)),
            // Texts are not transfers: nothing to track, and never written to the history.
            ServiceEvent::TextReceived(text) => self.emit("text-received", TextPayload::from(text)),
            // Straight onto the clipboard; the window only hears who sent it, never the text.
            ServiceEvent::ClipboardReceived(text) => {
                if let Some(state) = self.app.try_state::<AppState>() {
                    state.clipboard.write(&self.app, text.text);
                    self.emit("clipboard-received", ClipboardPayload { device_name: text.device_name });
                }
            }
            ServiceEvent::PairingPin { request_id, peer, pin } => {
                self.emit("pairing-pin", PinPayload::new(request_id, &peer, pin));
            }
            ServiceEvent::PairingResult { request_id, result, diagnosis } => {
                let (device, error) = match result {
                    Ok(device) => (Some(TrustedPayload::from(&device)), None),
                    Err(code) => (None, Some(CommandError::from(&code).with_diagnosis(diagnosis.as_ref()))),
                };
                self.emit("pairing-result", PairingResultPayload { request_id, device, error });
            }
        }
    }

    fn emit<T: Serialize + Clone>(&self, name: &str, payload: T) {
        if let Err(error) = self.app.emit(name, payload) {
            tracing::warn!(%error, name, "could not send an event to the window");
        }
    }

    /// Writes the history entry of a transfer that started; transfers that never started are not recorded.
    fn record(&mut self, id: TransferId, status: Status, error_code: Option<String>, path: Option<String>) {
        self.last_progress.remove(&id);
        let Some(info) = self.active.remove(&id) else { return };
        let entry = HistoryEntry {
            id: 0,
            direction: direction_name(info.direction).to_owned(),
            peer_name: info.peer_name,
            file_name: info.file_name,
            size: info.size,
            status,
            error_code,
            path,
            finished_at: 0,
        };
        let history: Option<Arc<History>> = self.history();
        if let Some(history) = history {
            tauri::async_runtime::spawn_blocking(move || {
                if let Err(error) = history.record(&entry) {
                    tracing::warn!(%error, "could not write the transfer history");
                }
            });
        }
    }

    fn history(&self) -> Option<Arc<History>> {
        use tauri::Manager;
        self.app.try_state::<crate::state::AppState>().map(|state| Arc::clone(&state.history))
    }
}
