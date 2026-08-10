//! `PipeWire` registry, stream, and graph adapter for Noire.

#![forbid(unsafe_code)]

#[cfg(feature = "pipewire-backend")]
mod connection;
mod registry;

#[cfg(feature = "pipewire-backend")]
pub use connection::{CoreFailure, PipewireConnection};
pub use registry::{
    AdvertisedFormat, DeviceAvailability, DeviceSelector, NodeDescriptor, NodeProperties,
    RESERVED_NODE_NAME, RegistrySnapshot,
};
