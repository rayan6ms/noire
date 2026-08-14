//! GTK presentation and asynchronous daemon control for Noire.

#![forbid(unsafe_code)]

#[cfg(feature = "gtk-ui")]
mod app;
#[cfg(feature = "gtk-ui")]
mod client;
#[cfg(feature = "gtk-ui")]
mod i18n;
pub mod state;

/// Runs the GTK application until its final window closes.
#[cfg(feature = "gtk-ui")]
pub fn run() {
    app::run();
}
