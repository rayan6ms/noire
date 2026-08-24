//! Binary-embedded artwork used by the GPUI shell.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// Keeps icons available to native packages, Flatpak, and an unintegrated `AppImage`.
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            "icons/noire.svg" => Some(include_bytes!("../../../icons/noire.svg")),
            "icons/noire-icon.svg" => Some(include_bytes!("../../../icons/noire-icon.svg")),
            "icons/window-dark.svg" => Some(WINDOW_DARK.as_bytes()),
            "icons/window-light.svg" => Some(WINDOW_LIGHT.as_bytes()),
            "icons/new-moon-emoji.svg" => Some(NEW_MOON_EMOJI.as_bytes()),
            "icons/full-moon-emoji.svg" => Some(FULL_MOON_EMOJI.as_bytes()),
            "icons/settings.svg" => Some(SETTINGS.as_bytes()),
            "icons/back.svg" => Some(BACK.as_bytes()),
            "icons/minimize.svg" => Some(MINIMIZE.as_bytes()),
            "icons/close.svg" => Some(CLOSE.as_bytes()),
            "icons/microphone.svg" => Some(MICROPHONE.as_bytes()),
            "icons/microphone-noisy.svg" => Some(MICROPHONE_NOISY.as_bytes()),
            "icons/microphone-clean.svg" => Some(MICROPHONE_CLEAN.as_bytes()),
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
            "noire-icon.svg",
            "window-dark.svg",
            "window-light.svg",
            "new-moon-emoji.svg",
            "full-moon-emoji.svg",
            "settings.svg",
            "back.svg",
            "minimize.svg",
            "close.svg",
            "microphone.svg",
            "microphone-noisy.svg",
            "microphone-clean.svg",
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
const MINIMIZE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M7 12h10"/></svg>"#;
const CLOSE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="m7 7 10 10M17 7 7 17"/></svg>"#;
const MICROPHONE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="2.5" width="6" height="12" rx="3"/><path d="M5.5 11.5a6.5 6.5 0 0 0 13 0M12 18v3M9 21h6"/></svg>"#;
const MICROPHONE_NOISY: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="5" width="6" height="10" rx="3" fill="currentColor" stroke="none"/><path d="M6.5 12a5.5 5.5 0 0 0 11 0M12 17.5V21M9.5 21h5"/><path d="m3 5 2 1.5L3.5 8 6 9M21 5l-2 1.5L20.5 8 18 9M7 2l1 2M17 2l-1 2"/></svg>"#;
const MICROPHONE_CLEAN: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="7" width="6" height="9" rx="3" fill="currentColor" stroke="none"/><path d="M6.5 13a5.5 5.5 0 0 0 11 0M12 18.5V22M9.5 22h5M9 4.5a4.25 4.25 0 0 1 6 0M7 2.5a7.1 7.1 0 0 1 10 0"/></svg>"#;
const WAVEFORM: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 12h2l2-6 3 12 3-12 2 9 2-3h4"/></svg>"#;
const SHIELD: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3 20 6v5c0 5-3.4 8.2-8 10-4.6-1.8-8-5-8-10V6l8-3Z"/><path d="m9 12 2 2 4-4"/></svg>"#;
const CHEVRON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="m9 6 6 6-6 6"/></svg>"#;
const RETRY: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M20 5v6h-6"/><path d="M19.1 15a8 8 0 1 1 .9-4"/></svg>"#;
const SPINNER: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M21 12a9 9 0 0 0-9-9"/><path opacity=".25" d="M12 3a9 9 0 1 0 9 9"/></svg>"#;
const WINDOW_DARK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 680 520" preserveAspectRatio="none"><rect x=".5" y=".5" width="679" height="519" rx="12.5" fill="#0a0a0a" stroke="#1e1e1e"/></svg>"##;
const WINDOW_LIGHT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 680 520" preserveAspectRatio="none"><rect x=".5" y=".5" width="679" height="519" rx="12.5" fill="#f1f1ef" stroke="#d4d4d0"/></svg>"##;
const NEW_MOON_EMOJI: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 36 36"><defs><radialGradient id="n" cx="35%" cy="28%" r="72%"><stop offset="0" stop-color="#454545"/><stop offset=".58" stop-color="#292929"/><stop offset="1" stop-color="#101010"/></radialGradient></defs><circle cx="18" cy="18" r="15.5" fill="url(#n)" stroke="#666"/><circle cx="12" cy="12" r="3.2" fill="#202020" opacity=".7"/><circle cx="23" cy="20" r="4.4" fill="#171717" opacity=".58"/><circle cx="15" cy="26" r="2.3" fill="#363636" opacity=".55"/></svg>"##;
const FULL_MOON_EMOJI: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 36 36"><defs><radialGradient id="f" cx="35%" cy="28%" r="72%"><stop offset="0" stop-color="#fff9cf"/><stop offset=".62" stop-color="#f1df9a"/><stop offset="1" stop-color="#cfb96e"/></radialGradient></defs><circle cx="18" cy="18" r="15.5" fill="url(#f)" stroke="#d5c27f"/><circle cx="12" cy="11" r="3.3" fill="#cfb96e" opacity=".62"/><path d="M20 5.4a7 7 0 0 1 6.2 4.3c-3.1.9-5.7.2-7.8-2.1z" fill="#e1cc80" opacity=".65"/><circle cx="24" cy="21" r="4.2" fill="#c7ae60" opacity=".52"/><circle cx="13" cy="26" r="2.4" fill="#d2bb6d" opacity=".58"/></svg>"##;
