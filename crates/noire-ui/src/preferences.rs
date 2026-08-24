//! Small desktop-only preferences kept separate from daemon audio settings.

use std::{env, fs, io, path::PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

const CONFIG_DIRECTORY: &str = "noire";
const CONFIG_FILE: &str = "ui.toml";

/// Which color scheme the desktop shell should use.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ThemePreference {
    /// Follow the desktop appearance reported by GPUI.
    #[default]
    System,
    /// Always use Noire's near-black palette.
    Dark,
    /// Always use Noire's lighter palette.
    Light,
}

impl ThemePreference {
    /// Resolves the saved preference against the current desktop appearance.
    pub(crate) const fn is_dark(self, system_is_dark: bool) -> bool {
        match self {
            Self::System => system_is_dark,
            Self::Dark => true,
            Self::Light => false,
        }
    }
}

/// Preferences that affect only the desktop shell.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct DesktopPreferences {
    /// The selected desktop color scheme. New installations follow the system.
    pub theme: ThemePreference,
    /// Hide the initial window and leave the tray item running.
    pub start_minimized: bool,
    /// Keep Noire running in the tray when the window is closed.
    pub close_to_tray: bool,
}

impl Default for DesktopPreferences {
    fn default() -> Self {
        Self {
            theme: ThemePreference::System,
            start_minimized: false,
            close_to_tray: true,
        }
    }
}

#[derive(Deserialize)]
struct StoredPreferences {
    theme: Option<ThemePreference>,
    // Compatibility with the 1.1.0 boolean preference.
    dark_theme: Option<bool>,
    start_minimized: Option<bool>,
    close_to_tray: Option<bool>,
}

impl<'de> Deserialize<'de> for DesktopPreferences {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let stored = StoredPreferences::deserialize(deserializer)?;
        let defaults = Self::default();
        Ok(Self {
            theme: stored.theme.unwrap_or_else(|| {
                stored.dark_theme.map_or(defaults.theme, |dark| {
                    if dark {
                        ThemePreference::Dark
                    } else {
                        ThemePreference::Light
                    }
                })
            }),
            start_minimized: stored.start_minimized.unwrap_or(defaults.start_minimized),
            close_to_tray: stored.close_to_tray.unwrap_or(defaults.close_to_tray),
        })
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
    use super::{DesktopPreferences, ThemePreference};

    #[test]
    fn defaults_keep_close_safe_and_window_visible() {
        let preferences = DesktopPreferences::default();
        assert_eq!(preferences.theme, ThemePreference::System);
        assert!(!preferences.start_minimized);
        assert!(preferences.close_to_tray);
    }

    #[test]
    fn missing_fields_receive_defaults() -> Result<(), toml::de::Error> {
        let preferences: DesktopPreferences = toml::from_str("start_minimized = true")?;
        assert_eq!(preferences.theme, ThemePreference::System);
        assert!(preferences.start_minimized);
        assert!(preferences.close_to_tray);
        Ok(())
    }

    #[test]
    fn legacy_boolean_theme_is_migrated() -> Result<(), toml::de::Error> {
        let dark: DesktopPreferences = toml::from_str("dark_theme = true")?;
        let light: DesktopPreferences = toml::from_str("dark_theme = false")?;
        assert_eq!(dark.theme, ThemePreference::Dark);
        assert_eq!(light.theme, ThemePreference::Light);
        Ok(())
    }

    #[test]
    fn system_theme_resolves_against_desktop_appearance() {
        assert!(ThemePreference::System.is_dark(true));
        assert!(!ThemePreference::System.is_dark(false));
        assert!(ThemePreference::Dark.is_dark(false));
        assert!(!ThemePreference::Light.is_dark(true));
    }
}
