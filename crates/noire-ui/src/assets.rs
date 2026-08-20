//! Binary-embedded artwork used by the GPUI shell.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// Keeps icons available to native packages, Flatpak, and an unintegrated `AppImage`.
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            "icons/noire.svg" => Some(include_bytes!("../../../icons/noire.svg")),
            "icons/settings.svg" => Some(SETTINGS.as_bytes()),
            "icons/back.svg" => Some(BACK.as_bytes()),
            "icons/moon.svg" => Some(MOON.as_bytes()),
            "icons/sun.svg" => Some(SUN.as_bytes()),
            "icons/minimize.svg" => Some(MINIMIZE.as_bytes()),
            "icons/close.svg" => Some(CLOSE.as_bytes()),
            "icons/microphone.svg" => Some(MICROPHONE.as_bytes()),
            "icons/waveform.svg" => Some(WAVEFORM.as_bytes()),
            "icons/shield.svg" => Some(SHIELD.as_bytes()),
            "icons/chevron.svg" => Some(CHEVRON.as_bytes()),
            "icons/retry.svg" => Some(RETRY.as_bytes()),
            "icons/spinner.svg" => Some(SPINNER.as_bytes()),
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        if path != "icons" {
            return Ok(Vec::new());
        }
        Ok([
            "noire.svg",
            "settings.svg",
            "back.svg",
            "moon.svg",
            "sun.svg",
            "minimize.svg",
            "close.svg",
            "microphone.svg",
            "waveform.svg",
            "shield.svg",
            "chevron.svg",
            "retry.svg",
            "spinner.svg",
        ]
        .into_iter()
        .map(SharedString::from)
        .collect())
    }
}

const SETTINGS: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z"/><path d="M19.4 15a1.7 1.7 0 0 0 .34 1.88l.06.06-2.83 2.83-.06-.06a1.7 1.7 0 0 0-1.88-.34 1.7 1.7 0 0 0-1.03 1.56V21h-4v-.08A1.7 1.7 0 0 0 8.94 19.4a1.7 1.7 0 0 0-1.88.34l-.06.06-2.83-2.83.06-.06A1.7 1.7 0 0 0 4.57 15 1.7 1.7 0 0 0 3 14H3v-4h.08A1.7 1.7 0 0 0 4.6 8.94a1.7 1.7 0 0 0-.34-1.88L4.2 7l2.83-2.83.06.06A1.7 1.7 0 0 0 9 4.57 1.7 1.7 0 0 0 10 3V3h4v.08A1.7 1.7 0 0 0 15.06 4.6a1.7 1.7 0 0 0 1.88-.34L17 4.2 19.83 7l-.06.06A1.7 1.7 0 0 0 19.43 9 1.7 1.7 0 0 0 21 10h.08v4H21a1.7 1.7 0 0 0-1.6 1Z"/></svg>"#;
const BACK: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m15 18-6-6 6-6"/></svg>"#;
const MOON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M20.8 15.3A9 9 0 1 1 8.7 3.2 7 7 0 0 0 20.8 15.3Z"/></svg>"#;
const SUN: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><circle cx="12" cy="12" r="8.5"/><circle cx="9" cy="9" r="1.3"/><path d="M15 7.5c1 .5 1.6 1.2 2 2M8 15.5c1.2.8 2.8 1.1 4.2.7" stroke-linecap="round"/></svg>"#;
const MINIMIZE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M7 12h10"/></svg>"#;
const CLOSE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="m7 7 10 10M17 7 7 17"/></svg>"#;
const MICROPHONE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="2.5" width="6" height="12" rx="3"/><path d="M5.5 11.5a6.5 6.5 0 0 0 13 0M12 18v3M9 21h6"/></svg>"#;
const WAVEFORM: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 12h2l2-6 3 12 3-12 2 9 2-3h4"/></svg>"#;
const SHIELD: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3 20 6v5c0 5-3.4 8.2-8 10-4.6-1.8-8-5-8-10V6l8-3Z"/><path d="m9 12 2 2 4-4"/></svg>"#;
const CHEVRON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="m9 6 6 6-6 6"/></svg>"#;
const RETRY: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M20 7v5h-5"/><path d="M19 12a7 7 0 1 0-2 5"/></svg>"#;
const SPINNER: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M21 12a9 9 0 0 0-9-9"/><path opacity=".25" d="M12 3a9 9 0 1 0 9 9"/></svg>"#;
