//! Bounded model creation and processing failures.

use core::fmt;

/// A model instance creation failure.
///
/// This error contains no heap-backed message. Detailed diagnostics belong in
/// the control plane that invokes the factory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreateError {
    /// Required model data is not available.
    ModelUnavailable,
    /// Model data or configuration is invalid.
    InvalidModel,
    /// The adapter could not initialize a usable instance.
    InitializationFailed,
}

impl fmt::Display for CreateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ModelUnavailable => "model data is unavailable",
            Self::InvalidModel => "model data or configuration is invalid",
            Self::InitializationFailed => "model initialization failed",
        })
    }
}

impl std::error::Error for CreateError {}

/// A synchronous frame-processing failure.
///
/// The fieldless representation is `Copy`, non-allocating, and suitable for a
/// real-time error path. Counters and detailed diagnostics live outside it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessError {
    /// The input slice does not match the descriptor's exact frame length.
    InputFrameLength,
    /// The output slice does not match the descriptor's exact frame length.
    OutputFrameLength,
    /// The input contains NaN or infinity.
    NonFiniteInput,
    /// Model output contains NaN or infinity.
    NonFiniteOutput,
    /// Frame statistics are non-finite or outside their declared range.
    InvalidStatistics,
    /// The concrete model reported an internal processing failure.
    ModelFailure,
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InputFrameLength => "input does not contain one exact model frame",
            Self::OutputFrameLength => "output does not contain one exact model frame",
            Self::NonFiniteInput => "model input contains a non-finite sample",
            Self::NonFiniteOutput => "model output contains a non-finite sample",
            Self::InvalidStatistics => "model statistics are invalid",
            Self::ModelFailure => "model processing failed",
        })
    }
}

impl std::error::Error for ProcessError {}
