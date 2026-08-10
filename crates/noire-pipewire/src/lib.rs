//! `PipeWire` registry, stream, and graph adapter for Noire.

#![forbid(unsafe_code)]

#[cfg(feature = "pipewire-backend")]
mod connection;

#[cfg(feature = "pipewire-backend")]
pub use connection::{CoreFailure, PipewireConnection};
