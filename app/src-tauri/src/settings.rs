//! App-only settings in `app-settings.json` next to `config.toml`: language, theme, close to tray, start at login.
//! Network settings stay in the core `config.toml`, shared with the CLI.

use std::{fs, io, path::Path};

use lanshare_core::paths;
use serde::{Deserialize, Serialize};
use tauri::Theme;

/// File name inside `paths::config_dir()`.
pub const SETTINGS_FILE_NAME: &str = "app-settings.json";

/// Settings that only the desktop app uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    /// `en`, `km` or `ar`; `None` follows the operating system's language.
    pub language: Option<String>,
    /// Light or dark; `None` follows the operating system's appearance.
    pub theme: Option<Theme>,
    /// Closing the window hides it to the tray, and receiving continues.
    pub close_to_tray: bool,
    /// Start the app when the user logs in.
    pub start_at_login: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self { language: None, theme: None, close_to_tray: true, start_at_login: false }
    }
}

impl AppSettings {
    /// Loads the settings, or the defaults if the file is missing or unreadable. Blocking.
    pub fn load() -> Self {
        let Ok(dir) = paths::config_dir() else { return Self::default() };
        Self::load_from(&dir.join(SETTINGS_FILE_NAME))
    }

    fn load_from(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
                tracing::warn!(%error, "app-settings.json is not valid, using defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Saves the settings. Blocking.
    ///
    /// # Errors
    ///
    /// When the file cannot be written.
    pub fn save(&self) -> io::Result<()> {
        let dir = paths::config_dir().map_err(io::Error::other)?;
        self.save_to(&dir.join(SETTINGS_FILE_NAME))
    }

    fn save_to(&self, path: &Path) -> io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        // A temporary file and a rename, so a crash never leaves a half-written file.
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, text)?;
        fs::rename(temporary, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_default_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SETTINGS_FILE_NAME);
        assert_eq!(AppSettings::load_from(&path), AppSettings::default());
        let settings = AppSettings {
            language: Some("km".into()),
            theme: Some(Theme::Dark),
            close_to_tray: false,
            start_at_login: true,
        };
        settings.save_to(&path).unwrap();
        assert_eq!(AppSettings::load_from(&path), settings);
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(AppSettings::load_from(&path), AppSettings::default());
    }

    #[test]
    fn settings_saved_before_themes_existed_follow_the_system() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SETTINGS_FILE_NAME);
        fs::write(&path, r#"{ "language": "ar", "closeToTray": false, "startAtLogin": false }"#).unwrap();
        let settings = AppSettings::load_from(&path);
        assert_eq!(settings.language.as_deref(), Some("ar"));
        assert_eq!(settings.theme, None);
    }
}
