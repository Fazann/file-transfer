//! The single source of every location `LanShare` stores files in, so the CLI and the app share one identity, trust
//! list and config.
//!
//! These functions create the directory on first use with `std::fs`, which blocks. In async code, call them inside
//! `tokio::task::spawn_blocking`.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use directories::ProjectDirs;

use crate::error::{Error, Result};

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("com", "LanShare", "LanShare")
        .ok_or_else(|| Error::Io(io::Error::new(io::ErrorKind::NotFound, "no home directory")))
}

/// Directory for `config.toml`.
///
/// # Errors
///
/// [`Error::Io`] if there is no home directory or the directory cannot be created.
pub fn config_dir() -> Result<PathBuf> {
    ensure_dir(project_dirs()?.config_dir())
}

/// Directory for persistent data, such as resume state and trusted devices.
///
/// # Errors
///
/// [`Error::Io`] if there is no home directory or the directory cannot be created.
pub fn data_dir() -> Result<PathBuf> {
    ensure_dir(project_dirs()?.data_dir())
}

/// Directory for log files.
///
/// # Errors
///
/// [`Error::Io`] if there is no home directory or the directory cannot be created.
pub fn log_dir() -> Result<PathBuf> {
    ensure_dir(&project_dirs()?.data_local_dir().join("logs"))
}

fn ensure_dir(dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(dir).map_err(Error::Io)?;
    Ok(dir.to_owned())
}
