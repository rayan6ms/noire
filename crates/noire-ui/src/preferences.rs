//! Small desktop-only preferences kept separate from daemon audio settings.

use std::{env, fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};

const CONFIG_DIRECTORY: &str = "noire";
const CONFIG_FILE: &str = "ui.toml";

/// Preferences that affect only the desktop shell.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct DesktopPreferences {
    /// Use Noire's near-black palette instead of the optional light palette.
    pub dark_theme: bool,
    /// Hide the initial window and leave the tray item running.
    pub start_minimized: bool,
    /// Keep Noire running in the tray when the window is closed.
    pub close_to_tray: bool,
}

impl Default for DesktopPreferences {
    fn default() -> Self {
        Self {
            dark_theme: true,
            start_minimized: false,
            close_to_tray: true,
        }
    }
}

impl DesktopPreferences {
    /// Loads preferences, falling back safely when no file exists or it is invalid.
    pub fn load() -> Self {
        let Ok(contents) = fs::read_to_string(config_path()) else {
            return Self::default();
        };
        toml::from_str(&contents).unwrap_or_default()
    }

    /// Atomically enough for a tiny local preference file: write a sibling and rename it.
    pub fn save(&self) -> io::Result<()> {
        let path = config_path();
        let Some(parent) = path.parent() else {
            return Err(io::Error::other(
                "Noire configuration has no parent directory",
            ));
        };
        fs::create_dir_all(parent)?;
        let temporary = path.with_extension("toml.tmp");
        let contents =
            toml::to_string_pretty(self).map_err(|error| io::Error::other(error.to_string()))?;
        fs::write(&temporary, contents)?;
        fs::rename(temporary, path)
    }
}

fn config_path() -> PathBuf {
    if let Some(directory) = env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(directory)
            .join(CONFIG_DIRECTORY)
            .join(CONFIG_FILE);
    }
    env::var_os("HOME").map_or_else(
        || PathBuf::from(".").join(CONFIG_DIRECTORY).join(CONFIG_FILE),
        |home| {
            PathBuf::from(home)
                .join(".config")
                .join(CONFIG_DIRECTORY)
                .join(CONFIG_FILE)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::DesktopPreferences;

    #[test]
    fn defaults_keep_close_safe_and_window_visible() {
        let preferences = DesktopPreferences::default();
        assert!(preferences.dark_theme);
        assert!(!preferences.start_minimized);
        assert!(preferences.close_to_tray);
    }

    #[test]
    fn missing_fields_receive_defaults() -> Result<(), toml::de::Error> {
        let preferences: DesktopPreferences = toml::from_str("start_minimized = true")?;
        assert!(preferences.dark_theme);
        assert!(preferences.start_minimized);
        assert!(preferences.close_to_tray);
        Ok(())
    }
}
