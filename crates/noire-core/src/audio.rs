//! Platform-neutral control boundary for an audio backend.
//!
//! Callback audio remains inside the backend and pipeline adapters. This module
//! carries only low-rate lifecycle commands and compact events; it is not an
//! audio transport.

use std::{error::Error, fmt};

/// A lifecycle request sent from the daemon control plane to its audio backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum BackendCommand {
    /// Create and activate the configured audio graph.
    Start,
    /// Deactivate the graph and remove its streams and virtual source.
    Stop,
    /// Discard audio state from older generations and begin a fresh generation.
    Reset {
        /// Monotonically increasing generation chosen by the control plane.
        generation: u64,
    },
}

/// A compact fault reported by an audio backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum BackendFault {
    /// The media server or one of the configured streams disconnected.
    Disconnected,
    /// Backend buffer metadata did not describe an accessible audio region.
    MalformedBuffer,
    /// Processed audio could not be accepted without exceeding a fixed bound.
    Overflow,
    /// A source request could not be completely filled with processed audio.
    Underflow,
}

/// A low-rate state or fault event emitted by an audio backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum BackendEvent {
    /// The configured graph became available.
    Started,
    /// The graph was stopped and its published objects were removed.
    Stopped,
    /// Both callback sides have crossed into a new audio generation.
    Reset {
        /// Generation that is now active.
        generation: u64,
    },
    /// The backend observed a fault requiring control-plane policy.
    Fault(BackendFault),
}

/// Why a backend could not accept a control request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum BackendCommandError {
    /// The backend's fixed-capacity command path has no room for the request.
    Busy,
}

impl fmt::Display for BackendCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => formatter.write_str("the audio backend command queue is full"),
        }
    }
}

impl Error for BackendCommandError {}

/// Control-plane port implemented by the production and fake audio backends.
///
/// Implementations must preserve request and event order. This trait does not
/// require `Send` or `Sync`: a native backend may own thread-affine media-server
/// objects. Calls are made outside real-time process callbacks.
pub trait AudioBackend {
    /// Accepts a lifecycle request without waiting for the transition to finish.
    ///
    /// Completion or failure is reported through [`Self::poll_event`].
    ///
    /// # Errors
    ///
    /// Returns [`BackendCommandError::Busy`] when the backend's bounded command
    /// path cannot accept another request.
    fn request(&mut self, command: BackendCommand) -> Result<(), BackendCommandError>;

    /// Returns the next backend event, if one is ready.
    fn poll_event(&mut self) -> Option<BackendEvent>;
}

#[cfg(test)]
mod tests {
    use super::BackendCommandError;

    #[test]
    fn busy_error_has_stable_human_context() {
        assert_eq!(
            BackendCommandError::Busy.to_string(),
            "the audio backend command queue is full"
        );
    }
}
