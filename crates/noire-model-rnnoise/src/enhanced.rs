//! Experimental multi-frame enhancement path layered beside production `RNNoise`.

use noire_dsp::{LateReverbConfig, LateReverbReducer};
use noire_model::{
    CreateError, Denoiser, DenoiserFactory, FrameStats, ModelDescriptor, ModelDescriptorSpec,
    ProcessError, finalize_process_output, prepare_process_frame,
};

use crate::{RNNOISE_DELAY_SAMPLES, RNNOISE_FRAME_SAMPLES, RNNOISE_SAMPLE_RATE_HZ, RnnoiseFactory};

const ENHANCED_MODEL_VERSION: &str = "prototype-1/rnnoise-nnnoiseless-0.5.2";

/// Configuration for the opt-in multi-frame enhancement prototype.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnhancedRnnoiseConfig {
    /// Conservative strength of causal late-tail cancellation.
    pub dereverb_strength: f32,
}

impl Default for EnhancedRnnoiseConfig {
    fn default() -> Self {
        Self {
            dereverb_strength: 0.35,
        }
    }
}

/// Factory for the opt-in `RNNoise` plus multi-frame late-tail prototype.
///
/// This factory is intentionally separate from [`RnnoiseFactory`]. Noire's
/// production composition continues to construct the existing factory until
/// the enhanced path wins the frozen quality and real-time gates.
#[derive(Clone, Copy, Debug)]
pub struct EnhancedRnnoiseFactory {
    descriptor: ModelDescriptor,
    rnnoise: RnnoiseFactory,
    config: EnhancedRnnoiseConfig,
}

impl EnhancedRnnoiseFactory {
    /// Creates the experimental factory without changing production defaults.
    ///
    /// # Errors
    ///
    /// Returns [`CreateError::InitializationFailed`] for invalid configuration
    /// or descriptor construction failure.
    pub fn new(config: EnhancedRnnoiseConfig) -> Result<Self, CreateError> {
        if !config.dereverb_strength.is_finite() || !(0.0..=1.0).contains(&config.dereverb_strength)
        {
            return Err(CreateError::InitializationFailed);
        }
        let rnnoise = RnnoiseFactory::new()?;
        let descriptor = ModelDescriptor::new(ModelDescriptorSpec {
            id: "org.noire.experimental.multiframe-rnnoise",
            name: "Noire multi-frame RNNoise prototype",
            version: ENHANCED_MODEL_VERSION,
            license: "GPL-3.0-or-later AND BSD-3-Clause",
            sample_rate_hz: RNNOISE_SAMPLE_RATE_HZ,
            channels: 1,
            frame_samples: RNNOISE_FRAME_SAMPLES,
            hop_samples: RNNOISE_FRAME_SAMPLES,
            lookahead_samples: 0,
            delay_samples: RNNOISE_DELAY_SAMPLES,
        })
        .map_err(|_| CreateError::InitializationFailed)?;
        Ok(Self {
            descriptor,
            rnnoise,
            config,
        })
    }
}

impl DenoiserFactory for EnhancedRnnoiseFactory {
    fn descriptor(&self) -> &ModelDescriptor {
        &self.descriptor
    }

    fn create(&self) -> Result<Box<dyn Denoiser>, CreateError> {
        let inner = self.rnnoise.create()?;
        let dereverb = LateReverbReducer::new(LateReverbConfig {
            strength: self.config.dereverb_strength,
        });
        Ok(Box::new(EnhancedRnnoiseDenoiser {
            descriptor: self.descriptor,
            inner,
            dereverb,
            model_output: [0.0; RNNOISE_FRAME_SAMPLES],
            pending_vad: 0.0,
        }))
    }
}

struct EnhancedRnnoiseDenoiser {
    descriptor: ModelDescriptor,
    inner: Box<dyn Denoiser>,
    dereverb: LateReverbReducer,
    model_output: [f32; RNNOISE_FRAME_SAMPLES],
    pending_vad: f32,
}

impl Denoiser for EnhancedRnnoiseDenoiser {
    fn descriptor(&self) -> &ModelDescriptor {
        &self.descriptor
    }

    fn reset(&mut self) {
        self.inner.reset();
        self.dereverb.reset();
        self.model_output.fill(0.0);
        self.pending_vad = 0.0;
    }

    fn process_frame(
        &mut self,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<FrameStats, ProcessError> {
        prepare_process_frame(&self.descriptor, input, output)?;
        let stats = self.inner.process_frame(input, &mut self.model_output)?;

        let mut enhanced = [0.0; RNNOISE_FRAME_SAMPLES];
        self.dereverb
            .process_frame(&self.model_output, &mut enhanced, self.pending_vad);
        output.copy_from_slice(&enhanced);
        self.pending_vad = stats.vad_probability();
        finalize_process_output(output, stats)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_precision_loss, clippy::float_cmp)]

    use noire_model::DenoiserFactory;

    use super::{EnhancedRnnoiseConfig, EnhancedRnnoiseFactory};
    use crate::{RNNOISE_DELAY_SAMPLES, RNNOISE_FRAME_SAMPLES, RnnoiseFactory};

    #[test]
    fn enhanced_path_is_distinct_and_keeps_the_baseline_latency()
    -> Result<(), Box<dyn std::error::Error>> {
        let factory = EnhancedRnnoiseFactory::new(EnhancedRnnoiseConfig::default())?;
        assert_eq!(
            factory.descriptor().id(),
            "org.noire.experimental.multiframe-rnnoise"
        );
        assert_eq!(factory.descriptor().delay_samples(), RNNOISE_DELAY_SAMPLES);
        assert_eq!(factory.descriptor().lookahead_samples(), 0);
        Ok(())
    }

    #[test]
    fn invalid_strength_is_rejected_outside_the_callback() {
        assert!(
            EnhancedRnnoiseFactory::new(EnhancedRnnoiseConfig {
                dereverb_strength: f32::NAN,
            })
            .is_err()
        );
        assert!(
            EnhancedRnnoiseFactory::new(EnhancedRnnoiseConfig {
                dereverb_strength: 1.1,
            })
            .is_err()
        );
    }

    #[test]
    fn enhanced_frames_are_finite_bounded_and_resettable() -> Result<(), Box<dyn std::error::Error>>
    {
        let factory = EnhancedRnnoiseFactory::new(EnhancedRnnoiseConfig::default())?;
        let mut model = factory.create()?;
        let input = signal_frame();
        let mut first = [0.0; RNNOISE_FRAME_SAMPLES];
        let mut changed = [0.0; RNNOISE_FRAME_SAMPLES];
        let mut reset = [0.0; RNNOISE_FRAME_SAMPLES];
        let first_stats = model.process_frame(&input, &mut first)?;
        model.process_frame(&input, &mut changed)?;
        model.reset();
        let reset_stats = model.process_frame(&input, &mut reset)?;
        assert_eq!(first, reset);
        assert_eq!(first_stats, reset_stats);
        assert!(changed.iter().all(|sample| sample.is_finite()));
        assert!(changed.iter().all(|sample| (-1.0..=1.0).contains(sample)));
        Ok(())
    }

    #[test]
    fn sustained_clean_speech_stays_numerically_close_to_the_rnnoise_baseline()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut baseline = RnnoiseFactory::new()?.create()?;
        let mut enhanced =
            EnhancedRnnoiseFactory::new(EnhancedRnnoiseConfig::default())?.create()?;
        let mut baseline_output = [0.0; RNNOISE_FRAME_SAMPLES];
        let mut enhanced_output = [0.0; RNNOISE_FRAME_SAMPLES];
        let mut maximum_delta = 0.0_f32;
        for frame_index in 0..64 {
            let input = varying_signal_frame(frame_index);
            let baseline_stats = baseline.process_frame(&input, &mut baseline_output)?;
            let enhanced_stats = enhanced.process_frame(&input, &mut enhanced_output)?;
            assert_eq!(enhanced_stats, baseline_stats);
            maximum_delta = maximum_delta.max(
                enhanced_output
                    .iter()
                    .zip(baseline_output.iter())
                    .map(|(candidate, reference)| (candidate - reference).abs())
                    .fold(0.0_f32, f32::max),
            );
        }
        assert!(
            maximum_delta <= 1.0e-5,
            "clean-stream delta {maximum_delta}"
        );
        Ok(())
    }

    fn signal_frame() -> [f32; RNNOISE_FRAME_SAMPLES] {
        varying_signal_frame(0)
    }

    fn varying_signal_frame(frame_index: usize) -> [f32; RNNOISE_FRAME_SAMPLES] {
        let mut frame = [0.0; RNNOISE_FRAME_SAMPLES];
        for (index, sample) in frame.iter_mut().enumerate() {
            let absolute = frame_index * RNNOISE_FRAME_SAMPLES + index;
            let fundamental = absolute as f32 * 2.0 * core::f32::consts::PI * 173.0 / 48_000.0;
            let harmonic = absolute as f32 * 2.0 * core::f32::consts::PI * 421.0 / 48_000.0;
            *sample = fundamental.sin() * 0.18 + harmonic.sin() * 0.07;
        }
        frame
    }
}
