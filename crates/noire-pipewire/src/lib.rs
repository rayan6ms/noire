//! `PipeWire` registry, stream, and graph adapter for Noire.

#![forbid(unsafe_code)]

mod bypass;
mod capture;
#[cfg(feature = "pipewire-backend")]
mod connection;
mod format;
#[cfg(feature = "pipewire-backend")]
mod graph;
mod live;
mod registry;
#[cfg(feature = "pipewire-backend")]
mod source;
#[cfg(feature = "native-test")]
mod synthetic;

pub use bypass::{
    BYPASS_RING_CAPACITY, BYPASS_STARTUP_QUANTA, BypassCaptureSink, BypassControl, BypassOutput,
    BypassOutputError, BypassOutputReport, BypassTelemetry, BypassTelemetrySnapshot,
    create_bypass_channel,
};
pub use capture::{
    CaptureBufferError, CaptureCounters, CaptureProcessor, CaptureReport, CaptureSink,
    CaptureTelemetry, CaptureTelemetrySnapshot, ChunkMetadata, InputGeneration,
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
#[cfg(feature = "pipewire-backend")]
pub use graph::{BypassGraph, BypassGraphError, BypassGraphService};
#[cfg(feature = "pipewire-backend")]
pub use graph::{GraphHealthIssue, LiveGraph, LiveGraphError};
pub use live::{
    DeadlinePolicy, FailMode, LiveCaptureSink, LiveControl, LivePipelineError, LiveState,
    LiveTelemetry, LiveTelemetrySnapshot, TimingHistogramSnapshot, create_live_channel,
};
pub use registry::{
    AdvertisedFormat, DeviceAvailability, DeviceSelector, InputResolution, InputUnavailable,
    NodeDescriptor, NodeProperties, REGISTRY_COALESCE_MILLIS, RESERVED_NODE_NAME, RegistryMonitor,
    RegistrySnapshot, SelectionPolicy,
};
#[cfg(feature = "pipewire-backend")]
pub use source::{
    CONSUMER_IDLE_DEBOUNCE, ConsumerDemand, DemandTransition, SourceStreamError, SourceStreamState,
    SourceTelemetry, SourceTelemetrySnapshot, VirtualSourceStream,
};
#[cfg(feature = "native-test")]
pub use synthetic::{
    SYNTHETIC_SOURCE_RATE, SyntheticSource, SyntheticSourceError, SyntheticSourceSpec,
    SyntheticSourceTelemetry,
};
