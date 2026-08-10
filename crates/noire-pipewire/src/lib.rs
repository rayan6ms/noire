//! `PipeWire` registry, stream, and graph adapter for Noire.

#![forbid(unsafe_code)]

mod capture;
#[cfg(feature = "pipewire-backend")]
mod connection;
mod format;
mod registry;
#[cfg(feature = "native-test")]
mod synthetic;

pub use capture::{
    CaptureBufferError, CaptureCounters, CaptureProcessor, CaptureReport, CaptureSink,
    CaptureTelemetry, CaptureTelemetrySnapshot, ChunkMetadata,
};
#[cfg(feature = "pipewire-backend")]
pub use capture::{
    CaptureStreamError, CaptureStreamState, NativeCaptureStream, NegotiatedFormatEvent,
};
#[cfg(feature = "pipewire-backend")]
pub use connection::{CoreFailure, PipewireConnection};
pub use format::{
    CANONICAL_CAPTURE_FORMAT, CaptureChannelPosition, CaptureFormat, CaptureSampleFormat,
    NegotiatedFormatError,
};
#[cfg(feature = "pipewire-backend")]
pub use format::{build_capture_format_pod, parse_negotiated_format};
pub use registry::{
    AdvertisedFormat, DeviceAvailability, DeviceSelector, InputResolution, InputUnavailable,
    NodeDescriptor, NodeProperties, REGISTRY_COALESCE_MILLIS, RESERVED_NODE_NAME, RegistryMonitor,
    RegistrySnapshot, SelectionPolicy,
};
#[cfg(feature = "native-test")]
pub use synthetic::{
    SYNTHETIC_SOURCE_RATE, SyntheticSource, SyntheticSourceError, SyntheticSourceTelemetry,
};
