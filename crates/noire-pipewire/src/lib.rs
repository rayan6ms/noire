//! `PipeWire` registry, stream, and graph adapter for Noire.

#![forbid(unsafe_code)]

#[cfg(feature = "pipewire-backend")]
mod connection;
mod registry;

#[cfg(feature = "pipewire-backend")]
pub use connection::{CoreFailure, PipewireConnection};
pub use registry::{
    AdvertisedFormat, DeviceAvailability, DeviceSelector, InputResolution, InputUnavailable,
    NodeDescriptor, NodeProperties, REGISTRY_COALESCE_MILLIS, RESERVED_NODE_NAME, RegistryMonitor,
    RegistrySnapshot, SelectionPolicy,
};
