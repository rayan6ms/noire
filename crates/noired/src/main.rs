//! Noire daemon process entry point.

#![forbid(unsafe_code)]

use clap::Parser;

/// Owns Noire's audio graph and control-plane state.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Arguments;

fn main() {
    Arguments::parse();
    println!("noired workspace skeleton; audio service not implemented yet");
}
