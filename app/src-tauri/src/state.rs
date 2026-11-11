//! [`AppState`]: everything the commands share, kept in Tauri's managed state. Identity, trust store and config come
//! from `lanshare_core::paths`, so the app and the CLI are the same device.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lanshare_core::{
    config::{Config, CONFIG_FILE_NAME},
    identity::{DeviceCertificate, TrustStore},
    paths,
    service::{LanShareService, ServiceOptions},
};
use tauri::AppHandle;
use tokio::sync::RwLock;

use crate::{
    clipboard::ClipboardSync,
    error::{CommandError, CommandResult},
    events,
    history::{History, HISTORY_FILE_NAME},
    settings::AppSettings,
};

/// Shared by every command.
pub struct AppState {
    service: RwLock<LanShareService>,
    /// This device's certificate.
    pub certificate: DeviceCertificate,
    /// Paired devices, shared with the CLI.
    pub trust: TrustStore,
    /// `config.toml`, as last saved.
    pub config: Mutex<Config>,
    /// `app-settings.json`, as last saved.
    pub settings: Mutex<AppSettings>,
    /// Finished transfers.
    pub history: Arc<History>,
    /// Clipboard sync with paired devices.
    pub clipboard: ClipboardSync,
}

impl AppState {
    /// Loads the device's identity and settings and starts the service.
    ///
    /// # Errors
    ///
    /// When the config, certificate, trust store or history cannot be opened, or the service cannot start.
    pub async fn init(app: &AppHandle) -> CommandResult<Self> {
        let config = Config::load().await?;
        let certificate = DeviceCertificate::load_or_create().await?;
        let trust = TrustStore::open().await?;
        let (settings, history) = tokio::task::spawn_blocking(|| {
            let path = paths::data_dir()?.join(HISTORY_FILE_NAME);
            let history = History::open(&path).map_err(|error| CommandError::io(&error))?;
            Ok::<_, CommandError>((AppSettings::load(), history))
        })
        .await
        .map_err(|error| CommandError::io(&error))??;
        let service = start_service(app, &config, &certificate, &trust).await?;
        Ok(Self {
            service: RwLock::new(service),
            certificate,
            trust,
            config: Mutex::new(config),
            settings: Mutex::new(settings),
            history: Arc::new(history),
            clipboard: ClipboardSync::default(),
        })
    }

    /// A handle to the running service.
    pub async fn service(&self) -> LanShareService {
        self.service.read().await.clone()
    }

    /// Stops the service and starts a new one with the current config, for settings that need it (device name,
    /// port, download folder). Running transfers are cancelled.
    ///
    /// # Errors
    ///
    /// When the new service cannot start, for example because the port is taken.
    pub async fn restart_service(&self, app: &AppHandle) -> CommandResult<()> {
        let mut service = self.service.write().await;
        service.shutdown().await;
        let config = lock(&self.config).clone();
        *service = start_service(app, &config, &self.certificate, &self.trust).await?;
        Ok(())
    }

    /// Saves `config` to `config.toml` and keeps it as the current config.
    ///
    /// # Errors
    ///
    /// When the file cannot be written.
    pub async fn save_config(&self, config: Config) -> CommandResult<()> {
        let to_save = config.clone();
        tokio::task::spawn_blocking(move || to_save.save_at(&paths::config_dir()?.join(CONFIG_FILE_NAME)))
            .await
            .map_err(|error| CommandError::io(&error))??;
        *lock(&self.config) = config;
        Ok(())
    }
}

async fn start_service(
    app: &AppHandle,
    config: &Config,
    certificate: &DeviceCertificate,
    trust: &TrustStore,
) -> CommandResult<LanShareService> {
    let options = ServiceOptions {
        config: config.clone(),
        certificate: certificate.clone(),
        trust: trust.clone(),
        mdns: true,
        port: None,
        download_dir: None,
        clipboard: true,
    };
    let (service, receiver) = LanShareService::start(options).await?;
    events::forward(app.clone(), service.clone(), receiver);
    Ok(service)
}

/// Locks a mutex, carrying on if another thread panicked while holding it: the values are only ever replaced
/// whole.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
