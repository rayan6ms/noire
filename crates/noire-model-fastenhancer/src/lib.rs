//! Production `FastEnhancer-B` 48 kHz adapter for Noire.

#![forbid(unsafe_code)]

use noire_model::{
    CreateError, Denoiser, DenoiserFactory, FrameStats, ModelDescriptor, ModelDescriptorSpec,
    ProcessError, finalize_process_output, prepare_process_frame,
};
use noire_model_fastenhancer_sys::State;

/// The sample rate required by `FastEnhancer-B`.
pub const FASTENHANCER_SAMPLE_RATE_HZ: u32 = 48_000;
/// The exact streaming hop and frame size.
pub const FASTENHANCER_FRAME_SAMPLES: usize = 512;
/// The overlap-add history delay measured for the streaming runtime.
pub const FASTENHANCER_DELAY_SAMPLES: usize = 512;
/// Stable production model identifier.
pub const FASTENHANCER_MODEL_ID: &str = "org.noire.fastenhancer.base-48khz";
/// SHA-256 of the embedded `fe_base_48k.bin` artifact.
pub const FASTENHANCER_WEIGHTS_SHA256: &str =
    "a3f475e6ae0cfbe337a411f4f2d01b0cdc49a3fbf1eed02ad46dd355074d0071";

const MODEL_VERSION: &str = "fastenhancer-b-48khz-a3f475e6ae0c";
const WEIGHTS: &[u8] = include_bytes!("../models/fe_base_48k.bin");

/// Factory for the qualified `FastEnhancer-B` 48 kHz model.
#[derive(Clone, Copy, Debug)]
pub struct FastEnhancerFactory {
    descriptor: ModelDescriptor,
}

impl FastEnhancerFactory {
    /// Creates the fixed production descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`CreateError::InitializationFailed`] only if the compile-time
    /// descriptor violates the shared model contract.
    pub fn new() -> Result<Self, CreateError> {
        let descriptor = ModelDescriptor::new(ModelDescriptorSpec {
            id: FASTENHANCER_MODEL_ID,
            name: "FastEnhancer-B 48 kHz",
            version: MODEL_VERSION,
            license: "MIT",
            sample_rate_hz: FASTENHANCER_SAMPLE_RATE_HZ,
            channels: 1,
            frame_samples: FASTENHANCER_FRAME_SAMPLES,
            hop_samples: FASTENHANCER_FRAME_SAMPLES,
            lookahead_samples: 0,
            delay_samples: FASTENHANCER_DELAY_SAMPLES,
        })
        .map_err(|_| CreateError::InitializationFailed)?;
        Ok(Self { descriptor })
    }
}

impl DenoiserFactory for FastEnhancerFactory {
    fn descriptor(&self) -> &ModelDescriptor {
        &self.descriptor
    }

    fn create(&self) -> Result<Box<dyn Denoiser>, CreateError> {
        let state = State::new(WEIGHTS).map_err(|_| CreateError::InvalidModel)?;
        Ok(Box::new(FastEnhancerDenoiser {
            descriptor: self.descriptor,
            state,
            input: [0.0; FASTENHANCER_FRAME_SAMPLES],
            raw_output: [0.0; FASTENHANCER_FRAME_SAMPLES],
        }))
    }
}

struct FastEnhancerDenoiser {
    descriptor: ModelDescriptor,
    state: State,
    input: [f32; FASTENHANCER_FRAME_SAMPLES],
    raw_output: [f32; FASTENHANCER_FRAME_SAMPLES],
}

impl Denoiser for FastEnhancerDenoiser {
    fn descriptor(&self) -> &ModelDescriptor {
        &self.descriptor
    }

    fn reset(&mut self) {
        self.state.reset();
        self.input.fill(0.0);
        self.raw_output.fill(0.0);
    }

    fn process_frame(
        &mut self,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<FrameStats, ProcessError> {
        prepare_process_frame(&self.descriptor, input, output)?;
        self.input.copy_from_slice(input);
        self.state
            .process(&self.input, &mut self.raw_output)
            .map_err(|_| ProcessError::ModelFailure)?;

        output.copy_from_slice(&self.raw_output);
        finalize_process_output(output, FrameStats::silence())?;
        let activity = output_activity_probability(output);
        FrameStats::new(activity)
    }
}

fn output_activity_probability(output: &[f32]) -> f32 {
    let mean_square = output
        .iter()
        .map(|sample| sample.clamp(-1.0, 1.0).powi(2))
        .sum::<f32>()
        / 512.0;
    ((mean_square - 1.0e-8) / (1.0e-4 - 1.0e-8)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use std::error::Error;

    use noire_model::DenoiserFactory;

    use super::{
        FASTENHANCER_DELAY_SAMPLES, FASTENHANCER_FRAME_SAMPLES, FASTENHANCER_MODEL_ID,
        FASTENHANCER_SAMPLE_RATE_HZ, FastEnhancerFactory,
    };

    #[test]
    fn descriptor_matches_the_qualified_streaming_contract() -> Result<(), Box<dyn Error>> {
        let factory = FastEnhancerFactory::new()?;
        let descriptor = factory.descriptor();
        assert_eq!(descriptor.id(), FASTENHANCER_MODEL_ID);
        assert_eq!(descriptor.sample_rate_hz(), FASTENHANCER_SAMPLE_RATE_HZ);
        assert_eq!(descriptor.frame_samples(), FASTENHANCER_FRAME_SAMPLES);
        assert_eq!(descriptor.hop_samples(), FASTENHANCER_FRAME_SAMPLES);
        assert_eq!(descriptor.delay_samples(), FASTENHANCER_DELAY_SAMPLES);
        assert_eq!(descriptor.license(), "MIT");
        Ok(())
    }

    #[test]
    fn output_is_finite_and_reset_is_deterministic() -> Result<(), Box<dyn Error>> {
        let factory = FastEnhancerFactory::new()?;
        let mut model = factory.create()?;
        let input = [0.0; FASTENHANCER_FRAME_SAMPLES];
        let mut first = [0.0; FASTENHANCER_FRAME_SAMPLES];
        let mut changed = [0.0; FASTENHANCER_FRAME_SAMPLES];
        let mut after_reset = [0.0; FASTENHANCER_FRAME_SAMPLES];
        model.process_frame(&input, &mut first)?;
        model.process_frame(&input, &mut changed)?;
        model.reset();
        model.process_frame(&input, &mut after_reset)?;
        assert_eq!(after_reset, first);
        assert!(first.iter().all(|sample| sample.is_finite()));
        Ok(())
    }
}
