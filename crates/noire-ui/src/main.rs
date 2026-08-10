//! Noire GTK application entry point.

#![forbid(unsafe_code)]

use clap::Parser;

/// Displays and controls the Noire daemon.
#[derive(Debug, Parser)]
#[command(name = "noire", version, about)]
struct Arguments;

fn main() {
    Arguments::parse();
    println!("noire workspace skeleton; GTK interface not implemented yet");
}
