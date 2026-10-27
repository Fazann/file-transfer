//! `config.toml`: device id, name, port, download folder and discovery settings.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use directories::UserDirs;
use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result},
    identity::{DeviceId, DeviceInfo, Os},
    paths,
    protocol::PROTOCOL_VERSION,
    transport::DEFAULT_PORT,
};

/// File name of the config inside [`paths::config_dir`].
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Longest device name in bytes. Keeps the name inside one mDNS TXT entry (255 bytes) and one UDP announcement.
const MAX_DEVICE_NAME_BYTES: usize = 200;

/// Keys every saved config contains. If one is missing, the file is rewritten with the default filled in.
const KEYS: [&str; 6] = ["device_id", "device_name", "port", "download_dir", "udp_fallback", "stale_part_days"];

/// Settings stored in `config.toml`.
///
/// `#[serde(default)]` fills any missing key from [`Config::default`], and unknown keys are ignored, so older and
/// newer config files keep loading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Stable id of this device. Generated on first run.
    pub device_id: DeviceId,
    /// Name shown to other devices.
    pub device_name: String,
    /// TCP port to receive transfers on.
    pub port: u16,
    /// Folder to save received files in.
    pub download_dir: PathBuf,
    /// Also announce and listen with UDP broadcast, for networks that block mDNS.
    pub udp_fallback: bool,
    /// Unfinished transfers (`.lanshare-part` and `.lanshare-state` files) untouched for this many days are deleted
    /// when the receiver starts.
    pub stale_part_days: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_id: DeviceId::new_random(),
            device_name: default_device_name(),
            port: DEFAULT_PORT,
            download_dir: default_download_dir(),
            udp_fallback: true,
            stale_part_days: 7,
        }
    }
}

impl Config {
    /// Loads `config.toml` from [`paths::config_dir`], creating it with defaults if it does not exist.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file cannot be read, parsed or written.
    pub async fn load() -> Result<Self> {
        // File I/O with `std::fs` blocks, so run it on tokio's blocking thread pool.
        tokio::task::spawn_blocking(|| Self::load_or_create_at(&paths::config_dir()?.join(CONFIG_FILE_NAME)))
            .await
            .map_err(|error| Error::Io(io::Error::other(error)))?
    }

    /// Loads the config at `path`, creating it with defaults if missing. Blocking.
    ///
    /// The file is rewritten only when it is new or lacks a key, so the generated device id is saved once and
    /// hand edits are otherwise left alone.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file cannot be read, parsed or written.
    pub fn load_or_create_at(path: &Path) -> Result<Self> {
        let (mut config, needs_save) = match fs::read_to_string(path) {
            Ok(text) => {
                let table: toml::Table = toml::from_str(&text).map_err(|error| invalid_config(path, &error))?;
                let missing_key = KEYS.iter().any(|key| !table.contains_key(*key));
                (table.try_into().map_err(|error| invalid_config(path, &error))?, missing_key)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (Self::default(), true),
            Err(error) => return Err(Error::Io(error)),
        };
        config.device_name = clean_device_name(&config.device_name);
        if needs_save {
            config.save_at(path)?;
        }
        Ok(config)
    }

    /// Writes the config to `path`. Blocking.
    ///
    /// Writes a temporary file first and renames it, so a crash never leaves a half-written config.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file cannot be written.
    pub fn save_at(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).map_err(|error| Error::Io(io::Error::other(error)))?;
        let temporary = path.with_extension("toml.tmp");
        fs::write(&temporary, text).map_err(Error::Io)?;
        fs::rename(&temporary, path).map_err(Error::Io)
    }

    /// [`Config::stale_part_days`] as a duration.
    #[must_use]
    pub fn stale_part_age(&self) -> Duration {
        Duration::from_secs(u64::from(self.stale_part_days) * 24 * 60 * 60)
    }

    /// What this device announces on the network.
    #[must_use]
    pub fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            id: self.device_id,
            name: self.device_name.clone(),
            os: Os::current(),
            port: self.port,
            protocol_version: PROTOCOL_VERSION,
        }
    }
}

fn invalid_config(path: &Path, error: &dyn std::error::Error) -> Error {
    Error::Io(io::Error::new(io::ErrorKind::InvalidData, format!("{}: {error}", path.display())))
}

/// The host name without a trailing `.local`, e.g. `Fazans-MacBook-Pro`.
fn default_device_name() -> String {
    let host = hostname::get().ok().and_then(|name| name.into_string().ok()).unwrap_or_default();
    let host = host.strip_suffix(".local").unwrap_or(&host);
    if host.trim().is_empty() {
        "lanshare-device".to_owned()
    } else {
        host.to_owned()
    }
}

/// The OS Downloads folder plus `LanShare`, falling back to `~/Downloads/LanShare`.
fn default_download_dir() -> PathBuf {
    let downloads = UserDirs::new().and_then(|dirs| {
        dirs.download_dir().map(Path::to_path_buf).or_else(|| Some(dirs.home_dir().join("Downloads")))
    });
    downloads.unwrap_or_else(|| PathBuf::from("downloads")).join("LanShare")
}

/// Removes control characters and surrounding spaces, and cuts the name to [`MAX_DEVICE_NAME_BYTES`] on a character
/// boundary. An empty result falls back to the host name.
fn clean_device_name(name: &str) -> String {
    let mut cleaned: String = name.chars().filter(|c| !c.is_control()).collect();
    if cleaned.len() > MAX_DEVICE_NAME_BYTES {
        let mut end = MAX_DEVICE_NAME_BYTES;
        while !cleaned.is_char_boundary(end) {
            end -= 1;
        }
        cleaned.truncate(end);
    }
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        default_device_name()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_created_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        let created = Config::load_or_create_at(&path).unwrap();
        assert!(path.exists());
        assert_eq!(created.port, DEFAULT_PORT);
        assert!(created.udp_fallback);
        assert_eq!(created.stale_part_days, 7);
        assert!(created.download_dir.ends_with("LanShare"));
        assert_eq!(Config::load_or_create_at(&path).unwrap(), created, "second load must keep the same id");
    }

    #[test]
    fn missing_keys_get_defaults_and_unknown_keys_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        fs::write(&path, "device_name = \"Office PC\"\nport = 6000\nfuture_setting = true\n").unwrap();
        let config = Config::load_or_create_at(&path).unwrap();
        assert_eq!(config.device_name, "Office PC");
        assert_eq!(config.port, 6000);
        assert_eq!(Config::load_or_create_at(&path).unwrap().device_id, config.device_id, "generated id was saved");
    }

    #[test]
    fn complete_file_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        Config::load_or_create_at(&path).unwrap();
        let edited = format!("# my comment\n{}", fs::read_to_string(&path).unwrap());
        fs::write(&path, &edited).unwrap();
        Config::load_or_create_at(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), edited);
    }

    #[test]
    fn broken_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        fs::write(&path, "port = \"not a number\"").unwrap();
        assert!(Config::load_or_create_at(&path).is_err());
    }

    #[test]
    fn device_names_are_cleaned() {
        assert_eq!(clean_device_name("  Kitchen\n Mac  "), "Kitchen Mac");
        // 100 three-byte Khmer characters = 300 bytes, cut to 66 whole characters (198 bytes).
        let long = clean_device_name(&"ក".repeat(100));
        assert_eq!(long.len(), 198);
        assert_ne!(clean_device_name("\n\t"), "");
    }
}
