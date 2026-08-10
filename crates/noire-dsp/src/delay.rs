//! Exact latency matching for the dry path.

use core::fmt;

use crate::{MAX_CALLBACK_FRAMES, MAX_DRY_DELAY_SAMPLES, SanitizeReport, sanitize_sample};

/// A dry-delay construction or processing error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DryDelayError {
    /// Requested latency exceeds one model frame plus one maximum quantum.
    DelayTooLarge,
    /// Input and output buffers have different lengths.
    ShapeMismatch,
    /// The callback quantum exceeds the fixed processing bound.
    QuantumTooLarge,
}

impl fmt::Display for DryDelayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::DelayTooLarge => "dry delay exceeds the fixed latency boundary",
            Self::ShapeMismatch => "dry delay input and output lengths differ",
            Self::QuantumTooLarge => "callback quantum exceeds the fixed dry-delay boundary",
        })
    }
}

impl std::error::Error for DryDelayError {}

/// An exact, bounded delay line that allocates only during construction.
#[derive(Clone, Debug)]
pub struct DryDelay {
    storage: Box<[f32]>,
    position: usize,
}

impl DryDelay {
    /// Allocates a zero-filled delay line outside the real-time processing path.
    ///
    /// # Errors
    ///
    /// Returns [`DryDelayError::DelayTooLarge`] if `delay_samples` exceeds the
    /// fixed dry-path latency budget.
    pub fn new(delay_samples: usize) -> Result<Self, DryDelayError> {
        if delay_samples > MAX_DRY_DELAY_SAMPLES {
            return Err(DryDelayError::DelayTooLarge);
        }
        Ok(Self {
            storage: vec![0.0; delay_samples].into_boxed_slice(),
            position: 0,
        })
    }

    /// Allocates the dry path for a pipeline delay plus one graph quantum.
    ///
    /// # Errors
    ///
    /// Returns [`DryDelayError::QuantumTooLarge`] if `quantum_samples` exceeds
    /// the callback bound, or [`DryDelayError::DelayTooLarge`] if the sum
    /// overflows or exceeds the fixed dry-path latency budget.
    pub fn for_pipeline(
        pipeline_delay_samples: usize,
        quantum_samples: usize,
    ) -> Result<Self, DryDelayError> {
        if quantum_samples > MAX_CALLBACK_FRAMES {
            return Err(DryDelayError::QuantumTooLarge);
        }
        let total = pipeline_delay_samples
            .checked_add(quantum_samples)
            .ok_or(DryDelayError::DelayTooLarge)?;
        Self::new(total)
    }

    /// Returns the exact latency in samples.
    #[must_use]
    pub fn delay_samples(&self) -> usize {
        self.storage.len()
    }

    /// Clears history without reallocating.
    pub fn reset(&mut self) {
        self.storage.fill(0.0);
        self.position = 0;
    }

    /// Delays one same-sized bounded chunk and returns sanitization counters.
    ///
    /// # Errors
    ///
    /// Returns an error when input/output lengths differ or the callback quantum
    /// exceeds [`MAX_CALLBACK_FRAMES`].
    pub fn process(
        &mut self,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<SanitizeReport, DryDelayError> {
        if input.len() != output.len() {
            return Err(DryDelayError::ShapeMismatch);
        }
        if input.len() > MAX_CALLBACK_FRAMES {
            return Err(DryDelayError::QuantumTooLarge);
        }

        let mut report = SanitizeReport::default();
        if self.storage.is_empty() {
            for (source, destination) in input.iter().zip(output.iter_mut()) {
                *destination = sanitize_sample(*source, &mut report);
            }
            return Ok(report);
        }

        for (source, destination) in input.iter().zip(output.iter_mut()) {
            *destination = self.storage[self.position];
            self.storage[self.position] = sanitize_sample(*source, &mut report);
            self.position += 1;
            if self.position == self.storage.len() {
                self.position = 0;
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_precision_loss, clippy::float_cmp)]

    use super::{DryDelay, DryDelayError};
    use proptest::prelude::*;

    #[test]
    fn produces_exact_sample_delay_across_chunks() -> Result<(), DryDelayError> {
        let mut delay = DryDelay::new(3)?;
        let mut first = [0.0; 2];
        let mut second = [0.0; 3];
        delay.process(&[1.0, 2.0], &mut first)?;
        delay.process(&[3.0, 4.0, 5.0], &mut second)?;
        assert_eq!(first, [0.0, 0.0]);
        assert_eq!(second, [0.0, 1.0, 2.0]);
        Ok(())
    }

    #[test]
    fn pipeline_constructor_adds_one_quantum() -> Result<(), DryDelayError> {
        let delay = DryDelay::for_pipeline(480, 256)?;
        assert_eq!(delay.delay_samples(), 736);
        Ok(())
    }

    #[test]
    fn zero_delay_is_sanitized_identity() -> Result<(), DryDelayError> {
        let mut delay = DryDelay::new(0)?;
        let mut output = [0.0; 3];
        let report = delay.process(&[0.25, -0.5, f32::NAN], &mut output)?;
        assert_eq!(output, [0.25, -0.5, 0.0]);
        assert_eq!(report.non_finite, 1);
        Ok(())
    }

    proptest! {
        #[test]
        fn arbitrary_chunking_matches_a_reference_delay(
            delay_samples in 0usize..=64,
            sizes in prop::collection::vec(0usize..=128, 0..32),
        ) {
            let constructed = DryDelay::new(delay_samples);
            prop_assert!(constructed.is_ok());
            let mut delay = match constructed {
                Ok(delay) => delay,
                Err(error) => return Err(TestCaseError::fail(error.to_string())),
            };
            let mut source_index = 0usize;
            for size in sizes {
                let input: Vec<f32> = (source_index..source_index + size)
                    .map(|index| index as f32 + 1.0)
                    .collect();
                let mut output = vec![0.0; size];
                let result = delay.process(&input, &mut output);
                prop_assert!(result.is_ok());
                for (offset, actual) in output.iter().enumerate() {
                    let absolute = source_index + offset;
                    let expected = if absolute < delay_samples {
                        0.0
                    } else {
                        (absolute - delay_samples) as f32 + 1.0
                    };
                    prop_assert_eq!(*actual, expected);
                }
                source_index += size;
            }
        }
    }
}
