//! Noire GTK application entry point.

#![forbid(unsafe_code)]

use clap::Parser;

/// Displays and controls the Noire daemon.
#[derive(Debug, Parser)]
#[command(name = "noire", version, about)]
struct Arguments;

fn main() {
    Arguments::parse();
    #[cfg(feature = "gtk-ui")]
    noire_ui::run();

    #[cfg(not(feature = "gtk-ui"))]
    eprintln!("noire was built without GTK support; rebuild with --features gtk-ui");
}
