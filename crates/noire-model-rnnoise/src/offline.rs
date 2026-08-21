//! Development-only sample runner for offline comparisons.

use core::fmt;

use noire_dsp::{FrameAssemblerError, MAX_CALLBACK_FRAMES};
use noire_model::{CreateError, Denoiser, DenoiserFactory, ProcessError};

use crate::{RNNOISE_FRAME_SAMPLES, RnnoiseFactory};

#[derive(Clone, Debug)]
struct RnnoiseFrameAssembler {
    storage: [f32; RNNOISE_FRAME_SAMPLES],
    pending: usize,
}

impl RnnoiseFrameAssembler {
    const fn new() -> Self {
        Self {
            storage: [0.0; RNNOISE_FRAME_SAMPLES],
            pending: 0,
        }
    }

    const fn pending_samples(&self) -> usize {
        self.pending
    }

    fn push(
        &mut self,
        input: &[f32],
        mut emit: impl FnMut(&[f32; RNNOISE_FRAME_SAMPLES]),
    ) -> Result<(), FrameAssemblerError> {
        if input.len() > MAX_CALLBACK_FRAMES {
            return Err(FrameAssemblerError::QuantumTooLarge);
        }

        let mut consumed = 0;
        while consumed < input.len() {
            let copied = (RNNOISE_FRAME_SAMPLES - self.pending).min(input.len() - consumed);
            self.storage[self.pending..self.pending + copied]
                .copy_from_slice(&input[consumed..consumed + copied]);
            self.pending += copied;
            consumed += copied;
            if self.pending == RNNOISE_FRAME_SAMPLES {
                emit(&self.storage);
                self.pending = 0;
            }
        }
        Ok(())
    }
}

/// A bounded failure from the development-only offline runner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OfflineError {
    /// The production adapter could not be created.
    Create(CreateError),
    /// The input contained NaN or infinity.
    NonFiniteInput,
    /// The production adapter rejected a frame.
    Process(ProcessError),
    /// One offline input chunk exceeded the production callback bound.
    Chunk(FrameAssemblerError),
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
            Self::Chunk(_) => "offline input chunk exceeds the frame-assembly boundary",
            Self::LengthOverflow => "offline sample length overflowed",
            Self::SampleConservation => "offline processing did not conserve samples",
        })
    }
}

impl std::error::Error for OfflineError {}

/// A development-only chunked runner around production framing and inference.
///
/// This type deliberately allocates output storage and is available only with
/// `offline-wav`; it must never be used from an audio callback.
pub struct OfflineDenoiser {
    assembler: RnnoiseFrameAssembler,
    denoiser: Box<dyn Denoiser>,
    model_output: [f32; RNNOISE_FRAME_SAMPLES],
    raw_output: Vec<f32>,
    accepted_samples: usize,
    delay_samples: usize,
}

impl OfflineDenoiser {
    /// Creates an empty offline stream using the production default adapter.
    ///
    /// # Errors
    ///
    /// Returns [`OfflineError::Create`] if adapter initialization fails.
    pub fn new() -> Result<Self, OfflineError> {
        let factory = RnnoiseFactory::new().map_err(OfflineError::Create)?;
        let delay_samples = factory.descriptor().delay_samples();
        let denoiser = factory.create().map_err(OfflineError::Create)?;
        Ok(Self {
            assembler: RnnoiseFrameAssembler::new(),
            denoiser,
            model_output: [0.0; RNNOISE_FRAME_SAMPLES],
            raw_output: Vec::new(),
            accepted_samples: 0,
            delay_samples,
        })
    }

    /// Accepts one bounded normalized mono chunk.
    ///
    /// # Errors
    ///
    /// Returns an error for non-finite input, an oversized chunk, length
    /// overflow, or a production adapter processing failure.
    pub fn push_chunk(&mut self, input: &[f32]) -> Result<(), OfflineError> {
        if input.iter().any(|sample| !sample.is_finite()) {
            return Err(OfflineError::NonFiniteInput);
        }
        self.accepted_samples = self
            .accepted_samples
            .checked_add(input.len())
            .ok_or(OfflineError::LengthOverflow)?;
        self.process_chunk(input)
    }

    /// Flushes the partial frame and declared delay, returning aligned output.
    ///
    /// # Errors
    ///
    /// Returns an error for adapter failure, arithmetic overflow, or a sample-
    /// conservation violation.
    pub fn finish(mut self) -> Result<Vec<f32>, OfflineError> {
        let pending = self.assembler.pending_samples();
        if pending != 0 {
            let padding = [0.0; RNNOISE_FRAME_SAMPLES];
            self.process_chunk(&padding[..RNNOISE_FRAME_SAMPLES - pending])?;
        }

        let mut remaining_delay = self.delay_samples;
        let silence = [0.0; RNNOISE_FRAME_SAMPLES];
        while remaining_delay != 0 {
            let pushed = remaining_delay.min(RNNOISE_FRAME_SAMPLES);
            self.process_chunk(&silence[..pushed])?;
            remaining_delay -= pushed;
        }
        let pending = self.assembler.pending_samples();
        if pending != 0 {
            self.process_chunk(&silence[..RNNOISE_FRAME_SAMPLES - pending])?;
        }

        let output_end = self
            .delay_samples
            .checked_add(self.accepted_samples)
            .ok_or(OfflineError::LengthOverflow)?;
        let aligned = self
            .raw_output
            .get(self.delay_samples..output_end)
            .ok_or(OfflineError::SampleConservation)?;
        if aligned.len() != self.accepted_samples {
            return Err(OfflineError::SampleConservation);
        }
        Ok(aligned.to_vec())
    }

    fn process_chunk(&mut self, input: &[f32]) -> Result<(), OfflineError> {
        let denoiser = &mut self.denoiser;
        let model_output = &mut self.model_output;
        let raw_output = &mut self.raw_output;
        let mut process_error = None;
        self.assembler
            .push(input, |frame| {
                if process_error.is_some() {
                    return;
                }
                match denoiser.process_frame(frame, model_output) {
                    Ok(_) => raw_output.extend_from_slice(model_output),
                    Err(error) => process_error = Some(error),
                }
            })
            .map_err(OfflineError::Chunk)?;
        process_error.map_or(Ok(()), |error| Err(OfflineError::Process(error)))
    }
}

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
    let mut runner = OfflineDenoiser::new()?;
    for chunk in input.chunks(noire_dsp::MAX_CALLBACK_FRAMES) {
        runner.push_chunk(chunk)?;
    }
    runner.finish()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{OfflineDenoiser, OfflineError, denoise_latency_compensated};
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

    #[test]
    fn fixed_callback_sizes_match_single_chunk_golden() -> Result<(), OfflineError> {
        let input = deterministic_signal(4_937);
        let expected = denoise_latency_compensated(&input)?;
        let actual = run_schedule(&input, &[64, 128, 256, 480, 512])?;
        assert_eq!(actual.len(), input.len());
        assert_eq!(actual, expected);
        Ok(())
    }

    #[test]
    fn deterministic_golden_checkpoints_stay_within_tolerance() -> Result<(), OfflineError> {
        let input = deterministic_signal(1_440);
        let output = denoise_latency_compensated(&input)?;
        let checkpoints = [120, 479, 480, 719, 959, 1_200];
        let expected = [
            -0.142_987_03,
            0.198_347_66,
            0.191_082_84,
            -0.050_481_93,
            -0.136_548_94,
            0.213_196_29,
        ];
        for (index, expected) in checkpoints.into_iter().zip(expected) {
            assert!((output[index] - expected).abs() <= 1.0e-6);
        }
        Ok(())
    }

    proptest! {
        #[test]
        fn randomized_callback_schedules_are_sample_exact(
            sizes in prop::collection::vec(1usize..=4_096, 1..48),
        ) {
            let input = deterministic_signal(1_913);
            let expected = denoise_latency_compensated(&input)
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            let actual = run_schedule(&input, &sizes)
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            prop_assert_eq!(actual.len(), input.len());
            prop_assert_eq!(actual, expected);
        }
    }

    fn run_schedule(input: &[f32], sizes: &[usize]) -> Result<Vec<f32>, OfflineError> {
        let mut runner = OfflineDenoiser::new()?;
        let mut offset = 0;
        let mut schedule = sizes.iter().copied().cycle();
        while offset < input.len() {
            let size = schedule.next().unwrap_or(1).min(input.len() - offset);
            runner.push_chunk(&input[offset..offset + size])?;
            offset += size;
        }
        runner.finish()
    }

    #[allow(clippy::cast_precision_loss)]
    fn deterministic_signal(samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|index| {
                let time = index as f32 / 48_000.0;
                let tone = (2.0 * core::f32::consts::PI * 731.0 * time).sin() * 0.22;
                let dither = ((index.wrapping_mul(1_103_515_245).wrapping_add(12_345) >> 16)
                    & 0x7fff) as f32
                    / 32_767.0
                    * 0.02
                    - 0.01;
                tone + dither
            })
            .collect()
    }
}
