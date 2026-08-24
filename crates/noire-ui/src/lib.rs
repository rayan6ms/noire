//! GPUI presentation and asynchronous daemon control for Noire.

#![forbid(unsafe_code)]

#[cfg(feature = "gpui-ui")]
mod app;
#[cfg(feature = "gpui-ui")]
mod assets;
#[cfg(feature = "gpui-ui")]
mod autostart;
#[cfg(feature = "gpui-ui")]
mod client;
#[cfg(feature = "gpui-ui")]
mod preferences;
pub mod state;
#[cfg(feature = "gpui-ui")]
mod tray;

/// Runs the GPUI application until the user exits it.
#[cfg(feature = "gpui-ui")]
pub fn run(start_minimized: bool) {
    app::run(start_minimized);
}
