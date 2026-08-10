//! Noire command-line client entry point.

#![forbid(unsafe_code)]

use clap::Parser;

/// Inspects and controls the Noire daemon.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Arguments;

fn main() {
    Arguments::parse();
    println!("noirectl workspace skeleton; daemon control not implemented yet");
}
