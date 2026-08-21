//! Arbitrary-quantum assembly into exact model frames.

use core::fmt;

use crate::{
    FRAME_ASSEMBLER_CAPACITY, MAX_CALLBACK_FRAMES, MODEL_FRAME_SAMPLES, ModelFrame, SanitizeReport,
    sanitize_sample,
};

/// A frame-assembly boundary error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameAssemblerError {
    /// The callback quantum exceeds the fixed processing bound.
    QuantumTooLarge,
}

impl fmt::Display for FrameAssemblerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("callback quantum exceeds the fixed frame-assembly boundary")
    }
}

impl std::error::Error for FrameAssemblerError {}

/// Counters from one assembler push.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FramePushReport {
    /// Input samples accepted.
    pub accepted_samples: usize,
    /// Exact model frames emitted synchronously.
    pub emitted_frames: usize,
    /// Invalid or denormal samples replaced with zero.
    pub sanitized: SanitizeReport,
}

/// A bounded assembler for exact 512-sample model frames.
#[derive(Clone, Debug)]
pub struct FrameAssembler {
    storage: [f32; FRAME_ASSEMBLER_CAPACITY],
    pending: usize,
}

impl Default for FrameAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameAssembler {
    /// Creates an empty assembler with fixed inline storage.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            storage: [0.0; FRAME_ASSEMBLER_CAPACITY],
            pending: 0,
        }
    }

    /// Returns the samples waiting for the next exact frame.
    #[must_use]
    pub const fn pending_samples(&self) -> usize {
        self.pending
    }

    /// Clears pending samples.
    pub fn reset(&mut self) {
        self.storage.fill(0.0);
        self.pending = 0;
    }

    /// Accepts one bounded callback chunk and synchronously emits exact frames.
    ///
    /// The callback must consume or copy each frame before returning; its
    /// reference does not escape this method.
    ///
    /// # Errors
    ///
    /// Returns [`FrameAssemblerError::QuantumTooLarge`] when `input` exceeds
    /// [`MAX_CALLBACK_FRAMES`].
    pub fn push(
        &mut self,
        input: &[f32],
        mut emit: impl FnMut(&ModelFrame),
    ) -> Result<FramePushReport, FrameAssemblerError> {
        if input.len() > MAX_CALLBACK_FRAMES {
            return Err(FrameAssemblerError::QuantumTooLarge);
        }

        let mut report = FramePushReport {
            accepted_samples: input.len(),
            ..FramePushReport::default()
        };
        let mut consumed = 0;
        while consumed < input.len() {
            let needed = MODEL_FRAME_SAMPLES - self.pending;
            let copied = needed.min(input.len() - consumed);
            for sample in &input[consumed..consumed + copied] {
                self.storage[self.pending] = sanitize_sample(*sample, &mut report.sanitized);
                self.pending += 1;
            }
            consumed += copied;

            if self.pending == MODEL_FRAME_SAMPLES {
                let mut frame = [0.0; MODEL_FRAME_SAMPLES];
                frame.copy_from_slice(&self.storage[..MODEL_FRAME_SAMPLES]);
                emit(&frame);
                report.emitted_frames += 1;
                self.pending = 0;
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameAssembler, FrameAssemblerError};
    use crate::MODEL_FRAME_SAMPLES;
    use proptest::prelude::*;

    #[test]
    fn emits_only_exact_frames_across_awkward_chunks() -> Result<(), FrameAssemblerError> {
        let mut assembler = FrameAssembler::new();
        let mut frames = 0;
        for size in [64, 128, 256, 480, 512] {
            let input = vec![0.25; size];
            assembler.push(&input, |frame| {
                assert_eq!(frame.len(), MODEL_FRAME_SAMPLES);
                frames += 1;
            })?;
        }
        let total = 64 + 128 + 256 + 480 + 512;
        assert_eq!(
            frames * MODEL_FRAME_SAMPLES + assembler.pending_samples(),
            total
        );
        Ok(())
    }

    proptest! {
        #[test]
        fn randomized_chunks_conserve_every_sample(sizes in prop::collection::vec(0usize..=4096, 0..64)) {
            let mut assembler = FrameAssembler::new();
            let mut emitted = 0usize;
            let mut accepted = 0usize;
            for size in sizes {
                let input = vec![0.125; size];
                let result = assembler.push(&input, |_| emitted += MODEL_FRAME_SAMPLES);
                prop_assert!(result.is_ok());
                accepted += size;
            }
            prop_assert_eq!(emitted + assembler.pending_samples(), accepted);
        }
    }
}
