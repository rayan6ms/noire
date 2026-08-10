//! Shared exact-frame boundary checks and model telemetry.

use crate::{ModelDescriptor, ProcessError};

/// Bounded telemetry returned for one successfully processed frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameStats {
    vad_probability: f32,
}

impl FrameStats {
    /// Validates one voice-activity probability in the inclusive range `[0, 1]`.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::InvalidStatistics`] for NaN, infinity, or a value
    /// outside the probability range.
    pub fn new(vad_probability: f32) -> Result<Self, ProcessError> {
        if !vad_probability.is_finite() || !(0.0..=1.0).contains(&vad_probability) {
            return Err(ProcessError::InvalidStatistics);
        }
        Ok(Self { vad_probability })
    }

    /// Returns a valid zero-probability reading.
    #[must_use]
    pub const fn silence() -> Self {
        Self {
            vad_probability: 0.0,
        }
    }

    /// Returns the voice-activity probability in `[0, 1]`.
    #[must_use]
    pub const fn vad_probability(self) -> f32 {
        self.vad_probability
    }
}

/// Clears output and validates exact frame shapes and finite input.
///
/// Concrete adapters should call this before changing recurrent model state.
/// Output is silence on every error path.
///
/// # Errors
///
/// Returns [`ProcessError::InputFrameLength`],
/// [`ProcessError::OutputFrameLength`], or [`ProcessError::NonFiniteInput`] when
/// the model boundary contract is not met.
pub fn prepare_process_frame(
    descriptor: &ModelDescriptor,
    input: &[f32],
    output: &mut [f32],
) -> Result<(), ProcessError> {
    output.fill(0.0);
    let expected = descriptor.frame_buffer_samples();
    if input.len() != expected {
        return Err(ProcessError::InputFrameLength);
    }
    if output.len() != expected {
        return Err(ProcessError::OutputFrameLength);
    }
    if input.iter().any(|sample| !sample.is_finite()) {
        return Err(ProcessError::NonFiniteInput);
    }
    Ok(())
}

/// Validates successful model output and flushes subnormal samples to zero.
///
/// Concrete adapters should return this result after inference. A non-finite
/// output clears the complete frame before returning an error.
///
/// # Errors
///
/// Returns [`ProcessError::NonFiniteOutput`] if any output sample is NaN or
/// infinity.
pub fn finalize_process_output(
    output: &mut [f32],
    stats: FrameStats,
) -> Result<FrameStats, ProcessError> {
    if output.iter().any(|sample| !sample.is_finite()) {
        output.fill(0.0);
        return Err(ProcessError::NonFiniteOutput);
    }
    for sample in output {
        if *sample != 0.0 && sample.is_subnormal() {
            *sample = 0.0;
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::{FrameStats, finalize_process_output, prepare_process_frame};
    use crate::{DescriptorError, ModelDescriptor, ModelDescriptorSpec, ProcessError};

    fn descriptor() -> Result<ModelDescriptor, DescriptorError> {
        ModelDescriptor::new(ModelDescriptorSpec {
            id: "test.frame",
            name: "Frame boundary fake",
            version: "1",
            license: "MIT",
            sample_rate_hz: 48_000,
            channels: 1,
            frame_samples: 2,
            hop_samples: 2,
            lookahead_samples: 0,
            delay_samples: 0,
        })
    }

    #[test]
    fn statistics_reject_invalid_probabilities() {
        assert_eq!(
            FrameStats::new(f32::NAN),
            Err(ProcessError::InvalidStatistics)
        );
        assert_eq!(FrameStats::new(-0.1), Err(ProcessError::InvalidStatistics));
        assert_eq!(FrameStats::new(1.1), Err(ProcessError::InvalidStatistics));
        assert_eq!(FrameStats::new(0.0), Ok(FrameStats::silence()));
        assert_eq!(
            FrameStats::new(1.0).map(FrameStats::vad_probability),
            Ok(1.0)
        );
        assert_eq!(FrameStats::silence().vad_probability(), 0.0);
    }

    #[test]
    fn prepare_rejects_bad_shapes_and_values_as_silence() -> Result<(), DescriptorError> {
        let descriptor = descriptor()?;
        let mut output = [1.0; 2];
        assert_eq!(
            prepare_process_frame(&descriptor, &[0.0], &mut output),
            Err(ProcessError::InputFrameLength)
        );
        assert_eq!(output, [0.0; 2]);

        output.fill(1.0);
        assert_eq!(
            prepare_process_frame(&descriptor, &[0.0, 0.0], &mut output[..1]),
            Err(ProcessError::OutputFrameLength)
        );
        assert_eq!(output, [0.0, 1.0]);

        output.fill(1.0);
        assert_eq!(
            prepare_process_frame(&descriptor, &[0.0, f32::INFINITY], &mut output),
            Err(ProcessError::NonFiniteInput)
        );
        assert_eq!(output, [0.0; 2]);
        Ok(())
    }

    #[test]
    fn finalize_rejects_non_finite_and_flushes_subnormal_output() -> Result<(), ProcessError> {
        let stats = FrameStats::new(0.5)?;
        let mut invalid = [0.25, f32::NAN];
        assert_eq!(
            finalize_process_output(&mut invalid, stats),
            Err(ProcessError::NonFiniteOutput)
        );
        assert_eq!(invalid, [0.0; 2]);

        let mut valid = [f32::from_bits(1), -0.25];
        assert_eq!(finalize_process_output(&mut valid, stats), Ok(stats));
        assert_eq!(valid, [0.0, -0.25]);
        Ok(())
    }
}
