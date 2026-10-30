//! "Export diagnostics": a zip of the logs and settings to attach to a bug report.
//!
//! Files get in through a list of what to include, so a new private file can never end up in the zip by accident.
//! `device.key` and `trusted_devices.toml` must stay out.

use std::{
    fs::{self, File},
    io,
    path::Path,
};

use lanshare_core::config::CONFIG_FILE_NAME;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

use crate::settings::SETTINGS_FILE_NAME;

/// Start of every log file name; the date and `.log` follow.
pub const LOG_FILE_PREFIX: &str = "lanshare";

/// Writes the zip to `destination`: every log file in `log_dir`, and the two settings files in `config_dir`.
///
/// # Errors
///
/// When a folder cannot be listed, a file cannot be read, or `destination` cannot be written.
pub fn export(destination: &Path, log_dir: &Path, config_dir: &Path) -> io::Result<()> {
    let mut zip = ZipWriter::new(File::create(destination)?);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for name in [CONFIG_FILE_NAME, SETTINGS_FILE_NAME] {
        let path = config_dir.join(name);
        // `app-settings.json` only exists once settings were saved.
        if path.is_file() {
            zip.start_file(name, options).map_err(io::Error::other)?;
            io::copy(&mut File::open(path)?, &mut zip)?;
        }
    }
    for entry in fs::read_dir(log_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_log = name.starts_with(LOG_FILE_PREFIX) && entry.path().extension().is_some_and(|ext| ext == "log");
        if is_log && entry.file_type()?.is_file() {
            zip.start_file(format!("logs/{name}"), options).map_err(io::Error::other)?;
            io::copy(&mut File::open(entry.path())?, &mut zip)?;
        }
    }
    zip.finish().map_err(io::Error::other)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use lanshare_core::identity::{KEY_FILE_NAME, TRUST_FILE_NAME};
    use zip::ZipArchive;

    use super::*;

    #[test]
    fn zip_holds_logs_and_settings_but_no_secrets() {
        let (logs, config, out) =
            (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for name in [CONFIG_FILE_NAME, SETTINGS_FILE_NAME, KEY_FILE_NAME, TRUST_FILE_NAME, "device.crt"] {
            fs::write(config.path().join(name), name).unwrap();
        }
        fs::write(logs.path().join("lanshare.2026-10-30.log"), "a line").unwrap();
        fs::write(logs.path().join("unrelated.txt"), "no").unwrap();

        let destination = out.path().join("diagnostics.zip");
        export(&destination, logs.path(), config.path()).unwrap();

        let archive = ZipArchive::new(File::open(destination).unwrap()).unwrap();
        let mut names: Vec<&str> = archive.file_names().collect();
        names.sort_unstable();
        assert_eq!(names, ["app-settings.json", "config.toml", "logs/lanshare.2026-10-30.log"]);
    }

    #[test]
    fn missing_app_settings_are_fine() {
        let (logs, config, out) =
            (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        export(&out.path().join("diagnostics.zip"), logs.path(), config.path()).unwrap();
    }
}
