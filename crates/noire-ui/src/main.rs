//! Noire GPUI application entry point.

#![forbid(unsafe_code)]

use clap::Parser;

/// Displays and controls the Noire daemon.
#[derive(Debug, Parser)]
#[command(name = "noire", version, about)]
struct Arguments {
    /// Start with the window hidden in the system tray.
    #[arg(long)]
    minimized: bool,
}

fn main() {
    let arguments = Arguments::parse();
    #[cfg(feature = "gpui-ui")]
    noire_ui::run(arguments.minimized);

    #[cfg(not(feature = "gpui-ui"))]
    {
        let _ = arguments;
        eprintln!("noire was built without GPUI support; rebuild with --features gpui-ui");
    }
}
