//! Control-plane composition of capture, bypass transport, and virtual source.

use std::time::Instant;

use crate::{
    BypassTelemetry, CaptureStreamError, ConsumerDemand, DemandTransition, NativeCaptureStream,
    PipewireConnection, SourceStreamError, SourceStreamState, VirtualSourceStream,
    create_bypass_channel,
};

/// Construction or demand-service failure for the bypass graph.
#[derive(Debug)]
pub enum BypassGraphError {
    /// The selected stable name was absent from the current physical registry.
    SelectedSourceUnavailable(String),
    /// Physical capture stream construction failed.
    Capture(CaptureStreamError),
    /// Virtual source stream construction failed.
    Source(SourceStreamError),
    /// `PipeWire` rejected capture activation/deactivation.
    Activation(pipewire::Error),
}

impl std::fmt::Display for BypassGraphError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SelectedSourceUnavailable(node_name) => {
                write!(formatter, "selected source is unavailable: {node_name}")
            }
            Self::Capture(error) => write!(formatter, "capture graph failed: {error}"),
            Self::Source(error) => write!(formatter, "virtual source graph failed: {error}"),
            Self::Activation(error) => write!(formatter, "capture activation failed: {error}"),
        }
    }
}

impl std::error::Error for BypassGraphError {}

/// Result of one demand-service poll.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BypassGraphService {
    /// No demand edge was due.
    #[default]
    Unchanged,
    /// Capture state was activated for a fresh generation.
    Activated,
    /// Capture was paused and all queued audio was cleared.
    Deactivated,
}

/// Phase-4 latency-matched bypass graph owned by one `PipeWire` control thread.
pub struct BypassGraph {
    capture: NativeCaptureStream,
    source: VirtualSourceStream,
    telemetry: BypassTelemetry,
}

impl BypassGraph {
    /// Connects the selected physical source to the stable Noire source.
    ///
    /// Capture is initially inactive and begins only from source stream demand.
    ///
    /// # Errors
    ///
    /// Returns the first capture/source construction error.
    pub fn connect(
        connection: &PipewireConnection,
        selected_node_name: &str,
    ) -> Result<Self, BypassGraphError> {
        let (sink, output, _control, telemetry) = create_bypass_channel();
        let selected_node_id = connection
            .registry_snapshot_now()
            .candidates()
            .iter()
            .find(|node| node.node_name == selected_node_name)
            .map(|node| node.global_id)
            .ok_or_else(|| {
                BypassGraphError::SelectedSourceUnavailable(selected_node_name.to_owned())
            })?;
        let capture = NativeCaptureStream::connect_with_sink_to_id(
            connection,
            selected_node_name,
            selected_node_id,
            sink,
            false,
        )
        .map_err(BypassGraphError::Capture)?;
        let source =
            VirtualSourceStream::connect(connection, output).map_err(BypassGraphError::Source)?;
        Ok(Self {
            capture,
            source,
            telemetry,
        })
    }

    /// Applies a source-demand edge on the owning control thread.
    ///
    /// # Errors
    ///
    /// Returns the native error if `PipeWire` rejects capture state change.
    pub fn service_demand(&self, now: Instant) -> Result<BypassGraphService, BypassGraphError> {
        match self.source.demand_transition_if_due(now) {
            Some(DemandTransition::Activate) => {
                self.source.clear_sensitive();
                let _ = self.capture.advance_input_generation();
                self.capture
                    .set_active(true)
                    .map_err(BypassGraphError::Activation)?;
                Ok(BypassGraphService::Activated)
            }
            Some(DemandTransition::Deactivate) => {
                self.capture
                    .set_active(false)
                    .map_err(BypassGraphError::Activation)?;
                let _ = self.capture.advance_input_generation();
                self.source.clear_sensitive();
                Ok(BypassGraphService::Deactivated)
            }
            None => {
                if self.source.demand() == ConsumerDemand::Active
                    && self.source.state() != SourceStreamState::Streaming
                {
                    self.source.discard_pending_sensitive();
                }
                Ok(BypassGraphService::Unchanged)
            }
        }
    }

    /// Returns the capture stream for state and format inspection.
    #[must_use]
    pub const fn capture(&self) -> &NativeCaptureStream {
        &self.capture
    }

    /// Returns the virtual source for state, demand, and format inspection.
    #[must_use]
    pub const fn source(&self) -> &VirtualSourceStream {
        &self.source
    }

    /// Returns lock-free transport telemetry.
    #[must_use]
    pub fn telemetry(&self) -> BypassTelemetry {
        self.telemetry.clone()
    }

    /// Returns whether a source consumer currently requires capture.
    #[must_use]
    pub fn demand(&self) -> ConsumerDemand {
        self.source.demand()
    }
}
