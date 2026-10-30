//! `trusted_devices.toml`: the devices this one has paired with, recognized by their certificate [`Fingerprint`].
//!
//! The CLI and the app share the file, and another process may change it while a receiver runs (for example
//! `lanshare trusted remove`). [`TrustStore::refresh`] picks such changes up.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use super::{DeviceId, Fingerprint};
use crate::{
    error::{Error, Result},
    paths,
};

/// File name of the trust store inside [`paths::config_dir`].
pub const TRUST_FILE_NAME: &str = "trusted_devices.toml";

/// A device this one paired with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedDevice {
    /// The device's stable id.
    pub id: DeviceId,
    /// Its name at the time of pairing.
    pub name: String,
    /// Its certificate fingerprint. This, not the id or name, is what is checked on every connection.
    pub fingerprint: Fingerprint,
    /// When the pairing happened, in seconds since 1970-01-01 UTC.
    pub paired_at: u64,
    /// Accept its transfers without asking. Off unless the user turns it on.
    #[serde(default)]
    pub auto_accept: bool,
}

impl TrustedDevice {
    /// A device paired just now, with `auto_accept` off.
    #[must_use]
    pub fn paired_now(id: DeviceId, name: String, fingerprint: Fingerprint) -> Self {
        let paired_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        Self { id, name, fingerprint, paired_at, auto_accept: false }
    }
}

/// How the file is laid out: one `[[devices]]` table per device.
#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustFile {
    #[serde(default)]
    devices: Vec<TrustedDevice>,
}

/// The trusted devices, kept in memory and saved to `trusted_devices.toml` on every change.
///
/// Cloning gives another handle to the same store, like copying a pointer in Go.
#[derive(Debug, Clone)]
pub struct TrustStore {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug)]
struct Inner {
    path: PathBuf,
    devices: Vec<TrustedDevice>,
    /// The file's stamp when it was last read or written, to notice changes by other processes.
    stamp: Option<FileStamp>,
}

impl TrustStore {
    /// Opens the store in [`paths::config_dir`]. A missing file is an empty store.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file exists but cannot be read or parsed.
    pub async fn open() -> Result<Self> {
        blocking(|| Self::open_at(&paths::config_dir()?.join(TRUST_FILE_NAME))).await
    }

    /// Opens the store at `path`. A missing file is an empty store. Blocking.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file exists but cannot be read or parsed.
    pub fn open_at(path: &Path) -> Result<Self> {
        let (devices, stamp) = read_file(path)?;
        Ok(Self { inner: Arc::new(Mutex::new(Inner { path: path.to_owned(), devices, stamp })) })
    }

    /// Every trusted device, in the order they were paired.
    #[must_use]
    pub fn list(&self) -> Vec<TrustedDevice> {
        self.lock().devices.clone()
    }

    /// The trusted device with this fingerprint, if any.
    #[must_use]
    pub fn get_by_fingerprint(&self, fingerprint: &Fingerprint) -> Option<TrustedDevice> {
        self.lock().devices.iter().find(|device| device.fingerprint == *fingerprint).cloned()
    }

    /// The trusted device with this id, if any.
    #[must_use]
    pub fn get_by_id(&self, id: DeviceId) -> Option<TrustedDevice> {
        self.lock().devices.iter().find(|device| device.id == id).cloned()
    }

    /// Adds a device, replacing any entry with the same id or fingerprint, and saves.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file cannot be written.
    pub async fn add(&self, device: TrustedDevice) -> Result<()> {
        self.change(move |devices| {
            devices.retain(|known| known.id != device.id && known.fingerprint != device.fingerprint);
            devices.push(device);
            true
        })
        .await
        .map(|_changed| ())
    }

    /// Removes the device with this id and saves. Returns whether it was there.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file cannot be written.
    pub async fn remove(&self, id: DeviceId) -> Result<bool> {
        self.change(move |devices| {
            let before = devices.len();
            devices.retain(|known| known.id != id);
            devices.len() != before
        })
        .await
    }

    /// Turns `auto_accept` on or off for the device with this id and saves. Returns whether it was there.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file cannot be written.
    pub async fn set_auto_accept(&self, id: DeviceId, auto_accept: bool) -> Result<bool> {
        self.change(move |devices| {
            let Some(device) = devices.iter_mut().find(|known| known.id == id) else { return false };
            device.auto_accept = auto_accept;
            true
        })
        .await
    }

    /// Re-reads the file if another process changed it since it was last read or written.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file changed and cannot be read or parsed. The old list is kept.
    pub async fn refresh(&self) -> Result<()> {
        let store = self.clone();
        blocking(move || store.refresh_blocking()).await
    }

    fn refresh_blocking(&self) -> Result<()> {
        let mut inner = self.lock();
        if file_stamp(&inner.path) != inner.stamp {
            (inner.devices, inner.stamp) = read_file(&inner.path)?;
        }
        Ok(())
    }

    /// Applies `edit` to a fresh copy of the list and saves it if `edit` returns `true`. Returns that value.
    async fn change(&self, edit: impl FnOnce(&mut Vec<TrustedDevice>) -> bool + Send + 'static) -> Result<bool> {
        let store = self.clone();
        blocking(move || {
            // Start from the file, so an edit made meanwhile by another process is not overwritten.
            store.refresh_blocking()?;
            let mut inner = store.lock();
            let mut devices = inner.devices.clone();
            if !edit(&mut devices) {
                return Ok(false);
            }
            write_file(&inner.path, &devices)?;
            inner.stamp = file_stamp(&inner.path);
            inner.devices = devices;
            Ok(true)
        })
        .await
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // Every update replaces the list in one assignment, so a panic elsewhere cannot leave it half-changed.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(work).await.map_err(|error| Error::Io(io::Error::other(error)))?
}

fn read_file(path: &Path) -> Result<(Vec<TrustedDevice>, Option<FileStamp>)> {
    let stamp = file_stamp(path);
    match fs::read_to_string(path) {
        Ok(text) => {
            let file: TrustFile = toml::from_str(&text).map_err(|error| {
                Error::Io(io::Error::new(io::ErrorKind::InvalidData, format!("{}: {error}", path.display())))
            })?;
            Ok((file.devices, stamp))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok((Vec::new(), None)),
        Err(error) => Err(Error::Io(error)),
    }
}

/// Writes a temporary file and renames it, so a crash never leaves a half-written store.
fn write_file(path: &Path, devices: &[TrustedDevice]) -> Result<()> {
    let text = toml::to_string_pretty(&TrustFile { devices: devices.to_vec() })
        .map_err(|error| Error::Io(io::Error::other(error)))?;
    let temporary = path.with_extension("toml.tmp");
    fs::write(&temporary, text).map_err(Error::Io)?;
    fs::rename(&temporary, path).map_err(Error::Io)
}

/// Modification time and size. The size catches changes within the timestamp resolution of older file systems.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified: SystemTime,
    len: u64,
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let metadata = fs::metadata(path).ok()?;
    Some(FileStamp { modified: metadata.modified().ok()?, len: metadata.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, byte: u8) -> TrustedDevice {
        TrustedDevice::paired_now(DeviceId::new_random(), name.into(), Fingerprint::from_bytes([byte; 32]))
    }

    #[tokio::test]
    async fn devices_are_saved_and_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TRUST_FILE_NAME);
        let store = TrustStore::open_at(&path).unwrap();
        assert_eq!(store.list(), Vec::new());

        let desk = device("Desk", 1);
        store.add(desk.clone()).await.unwrap();
        assert!(store.set_auto_accept(desk.id, true).await.unwrap());
        let reopened = TrustStore::open_at(&path).unwrap();
        let saved = reopened.get_by_fingerprint(&desk.fingerprint).unwrap();
        assert_eq!(saved.name, "Desk");
        assert!(saved.auto_accept);
        assert_eq!(reopened.get_by_id(desk.id), Some(saved));

        assert!(reopened.remove(desk.id).await.unwrap());
        assert!(!reopened.remove(desk.id).await.unwrap());
        assert_eq!(TrustStore::open_at(&path).unwrap().list(), Vec::new());
    }

    #[tokio::test]
    async fn pairing_again_replaces_the_old_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = TrustStore::open_at(&dir.path().join(TRUST_FILE_NAME)).unwrap();
        let old = device("Laptop", 1);
        store.add(old.clone()).await.unwrap();
        // Same device id with a new key, e.g. after reinstalling.
        let new = TrustedDevice { fingerprint: Fingerprint::from_bytes([2; 32]), ..old.clone() };
        store.add(new.clone()).await.unwrap();
        assert_eq!(store.list(), vec![new]);
        assert_eq!(store.get_by_fingerprint(&old.fingerprint), None);
    }

    #[tokio::test]
    async fn changes_by_another_process_are_picked_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TRUST_FILE_NAME);
        let receiver = TrustStore::open_at(&path).unwrap();
        let other_process = TrustStore::open_at(&path).unwrap();
        let phone = device("Phone", 3);
        receiver.add(phone.clone()).await.unwrap();

        other_process.refresh().await.unwrap();
        assert!(other_process.remove(phone.id).await.unwrap());
        receiver.refresh().await.unwrap();
        assert_eq!(receiver.get_by_fingerprint(&phone.fingerprint), None);
    }
}
