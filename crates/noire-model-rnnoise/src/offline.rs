//! Development-only sample runner for offline comparisons.

use core::fmt;

use noire_model::{CreateError, DenoiserFactory, ProcessError};

use crate::RnnoiseFactory;

/// A bounded failure from the development-only offline runner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OfflineError {
    /// The production adapter could not be created.
    Create(CreateError),
    /// The input contained NaN or infinity.
    NonFiniteInput,
    /// The production adapter rejected a frame.
    Process(ProcessError),
    /// Descriptor arithmetic exceeded the platform's addressable range.
    LengthOverflow,
    /// The output length differed from the input length.
    SampleConservation,
}

impl fmt::Display for OfflineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Create(_) => "RNNoise adapter creation failed",
            Self::NonFiniteInput => "offline input contains a non-finite sample",
            Self::Process(_) => "RNNoise frame processing failed",
            Self::LengthOverflow => "offline sample length overflowed",
            Self::SampleConservation => "offline processing did not conserve samples",
        })
    }
}

impl std::error::Error for OfflineError {}

/// Processes normalized mono samples through the production adapter.
///
/// The adapter's declared delay is removed for offline reference comparison.
/// The returned vector always has exactly `input.len()` samples, including when
/// the input ends with a partial model frame. This helper is feature-gated and
/// may allocate; it is not part of Noire's real-time processing path.
///
/// # Errors
///
/// Returns an error for non-finite input, adapter creation/processing failures,
/// arithmetic overflow, or an internal sample-conservation violation.
pub fn denoise_latency_compensated(input: &[f32]) -> Result<Vec<f32>, OfflineError> {
    if input.iter().any(|sample| !sample.is_finite()) {
        return Err(OfflineError::NonFiniteInput);
    }

    let factory = RnnoiseFactory::new().map_err(OfflineError::Create)?;
    let descriptor = *factory.descriptor();
    let frame_samples = descriptor.frame_buffer_samples();
    let delay_samples = descriptor.delay_samples();
    let required_output = input
        .len()
        .checked_add(delay_samples)
        .ok_or(OfflineError::LengthOverflow)?;
    let frame_count = required_output
        .checked_add(frame_samples - 1)
        .ok_or(OfflineError::LengthOverflow)?
        / frame_samples;

    let mut denoiser = factory.create().map_err(OfflineError::Create)?;
    let mut input_frame = vec![0.0; frame_samples];
    let mut output_frame = vec![0.0; frame_samples];
    let mut output = Vec::with_capacity(input.len());

    for frame_index in 0..frame_count {
        let frame_start = frame_index
            .checked_mul(frame_samples)
            .ok_or(OfflineError::LengthOverflow)?;
        input_frame.fill(0.0);
        if frame_start < input.len() {
            let copied = frame_samples.min(input.len() - frame_start);
            input_frame[..copied].copy_from_slice(&input[frame_start..frame_start + copied]);
        }

        denoiser
            .process_frame(&input_frame, &mut output_frame)
            .map_err(OfflineError::Process)?;

        for (offset, sample) in output_frame.iter().enumerate() {
            let absolute = frame_start
                .checked_add(offset)
                .ok_or(OfflineError::LengthOverflow)?;
            if absolute >= delay_samples && output.len() < input.len() {
                output.push(*sample);
            }
        }
    }

    if output.len() != input.len() {
        return Err(OfflineError::SampleConservation);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{OfflineError, denoise_latency_compensated};
    use crate::RNNOISE_FRAME_SAMPLES;

    #[test]
    fn empty_input_stays_empty() -> Result<(), OfflineError> {
        assert!(denoise_latency_compensated(&[])?.is_empty());
        Ok(())
    }

    #[test]
    fn partial_final_frame_conserves_finite_samples() -> Result<(), OfflineError> {
        let mut input = vec![0.0; RNNOISE_FRAME_SAMPLES + 37];
        input[17] = 0.5;
        let output = denoise_latency_compensated(&input)?;
        assert_eq!(output.len(), input.len());
        assert!(output.iter().all(|sample| sample.is_finite()));
        Ok(())
    }

    #[test]
    fn non_finite_input_is_rejected_before_processing() {
        assert_eq!(
            denoise_latency_compensated(&[f32::NAN]),
            Err(OfflineError::NonFiniteInput)
        );
    }
}
