//! Feature-gated `nnnoiseless` default-model implementation.

use nnnoiseless::{DenoiseState, RnnModel};
use noire_model::{
    CreateError, Denoiser, DenoiserFactory, FrameStats, ModelDescriptor, ModelDescriptorSpec,
    ProcessError, finalize_process_output, prepare_process_frame,
};

/// The sample rate required by `RNNoise`.
pub const RNNOISE_SAMPLE_RATE_HZ: u32 = 48_000;

/// The exact number of mono samples in one `RNNoise` frame.
pub const RNNOISE_FRAME_SAMPLES: usize = DenoiseState::FRAME_SIZE;

/// The documented one-frame startup/history delay.
pub const RNNOISE_DELAY_SAMPLES: usize = RNNOISE_FRAME_SAMPLES;

/// SHA-256 of `nnnoiseless` 0.5.2's embedded `src/weights.rnn`.
pub const DEFAULT_WEIGHTS_SHA256: &str =
    "e6de5fbfadf7ec91d1b24d6a6ccfd0290cb4d8bf555c5eab3ce41506f67a58b1";

/// SHA-256 of Noire's opt-in `VoiceBank` quality candidate.
pub const QUALITY_V1_WEIGHTS_SHA256: &str =
    "2f0958c50378499cbd8869723b7dc214a65873304ec81856d3c084c08c0e9048";

const MODEL_INPUT_POSITIVE_SCALE: f32 = 32_767.0;
const MODEL_INPUT_NEGATIVE_SCALE: f32 = 32_768.0;
const MODEL_VERSION: &str = "nnnoiseless-0.5.2/default-e6de5fbfadf7ec91";

/// A factory for independent `nnnoiseless` instances using embedded weights.
#[derive(Clone, Copy, Debug)]
pub struct RnnoiseFactory {
    descriptor: ModelDescriptor,
}

/// An opt-in factory for evaluating versioned, `nnnoiseless`-compatible weights.
///
/// This does not alter [`RnnoiseFactory`] or its embedded production fallback.
/// Candidate bytes are parsed and owned on the control path before any audio
/// callback is activated.
#[derive(Clone)]
pub struct RnnoiseCandidateFactory {
    descriptor: ModelDescriptor,
    model: RnnModel,
}

impl RnnoiseCandidateFactory {
    /// Parses one candidate in `nnnoiseless`'s compact binary format.
    ///
    /// The provisional license expression records the implementation license
    /// and the CC BY 4.0 VoiceBank--DEMAND source data used by Noire's current
    /// training experiment. A promoted model must replace the generic version
    /// with its immutable content hash and complete provenance record.
    ///
    /// # Errors
    ///
    /// Returns [`CreateError::InvalidModel`] if `bytes` do not contain one
    /// complete model with dimensions accepted by `nnnoiseless`.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CreateError> {
        let model = RnnModel::from_bytes(bytes).ok_or(CreateError::InvalidModel)?;
        let descriptor = ModelDescriptor::new(ModelDescriptorSpec {
            id: "org.noire.experimental.rnnoise-candidate",
            name: "Noire RNNoise training candidate",
            version: "unqualified-local-candidate",
            license: "BSD-3-Clause AND CC-BY-4.0",
            sample_rate_hz: RNNOISE_SAMPLE_RATE_HZ,
            channels: 1,
            frame_samples: RNNOISE_FRAME_SAMPLES,
            hop_samples: RNNOISE_FRAME_SAMPLES,
            lookahead_samples: 0,
            delay_samples: RNNOISE_DELAY_SAMPLES,
        })
        .map_err(|_| CreateError::InitializationFailed)?;
        Ok(Self { descriptor, model })
    }

    /// Loads the immutable Noire quality-v1 candidate embedded with this crate.
    ///
    /// This remains explicitly opt-in: `VoiceBank` speech-quality results improve,
    /// while the registered stress suite still shows a small mean STOI regression
    /// concentrated in procedural music. [`RnnoiseFactory`] therefore continues
    /// to provide the default and fallback model.
    ///
    /// # Errors
    ///
    /// Returns [`CreateError::InvalidModel`] if the embedded artifact is not a
    /// complete model accepted by `nnnoiseless`.
    pub fn quality_v1() -> Result<Self, CreateError> {
        let model = RnnModel::from_static_bytes(include_bytes!("../models/rnnoise-quality-v1.rnn"))
            .ok_or(CreateError::InvalidModel)?;
        let descriptor = ModelDescriptor::new(ModelDescriptorSpec {
            id: "org.noire.experimental.rnnoise-quality-v1",
            name: "Noire RNNoise quality v1 (opt-in)",
            version: "voicebank-quality-v1-2f0958c50378",
            license: "BSD-3-Clause AND CC-BY-4.0",
            sample_rate_hz: RNNOISE_SAMPLE_RATE_HZ,
            channels: 1,
            frame_samples: RNNOISE_FRAME_SAMPLES,
            hop_samples: RNNOISE_FRAME_SAMPLES,
            lookahead_samples: 0,
            delay_samples: RNNOISE_DELAY_SAMPLES,
        })
        .map_err(|_| CreateError::InitializationFailed)?;
        Ok(Self { descriptor, model })
    }
}

impl RnnoiseFactory {
    /// Creates and validates the fixed default-model descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`CreateError::InitializationFailed`] if the built-in descriptor
    /// violates the shared model contract.
    pub fn new() -> Result<Self, CreateError> {
        let descriptor = ModelDescriptor::new(ModelDescriptorSpec {
            id: "org.rnnoise.nnnoiseless.default",
            name: "RNNoise (nnnoiseless embedded default)",
            version: MODEL_VERSION,
            license: "BSD-3-Clause",
            sample_rate_hz: RNNOISE_SAMPLE_RATE_HZ,
            channels: 1,
            frame_samples: RNNOISE_FRAME_SAMPLES,
            hop_samples: RNNOISE_FRAME_SAMPLES,
            lookahead_samples: 0,
            delay_samples: RNNOISE_DELAY_SAMPLES,
        })
        .map_err(|_| CreateError::InitializationFailed)?;
        Ok(Self { descriptor })
    }
}

impl DenoiserFactory for RnnoiseFactory {
    fn descriptor(&self) -> &ModelDescriptor {
        &self.descriptor
    }

    fn create(&self) -> Result<Box<dyn Denoiser>, CreateError> {
        // Trigger nnnoiseless's lazy FFT/window state outside frame processing,
        // then return a clean recurrent state. The daemon must invoke creation on
        // its eventual processing thread before activating the stream.
        let mut warming_state = DenoiseState::new();
        let silence = [0.0; RNNOISE_FRAME_SAMPLES];
        let mut discarded = [0.0; RNNOISE_FRAME_SAMPLES];
        let _ = warming_state.process_frame(&mut discarded, &silence);

        Ok(Box::new(RnnoiseDenoiser {
            descriptor: self.descriptor,
            state: DenoiseState::new(),
            model_input: [0.0; RNNOISE_FRAME_SAMPLES],
            model_output: [0.0; RNNOISE_FRAME_SAMPLES],
        }))
    }
}

impl DenoiserFactory for RnnoiseCandidateFactory {
    fn descriptor(&self) -> &ModelDescriptor {
        &self.descriptor
    }

    fn create(&self) -> Result<Box<dyn Denoiser>, CreateError> {
        let mut warming_state = DenoiseState::from_model(self.model.clone());
        let silence = [0.0; RNNOISE_FRAME_SAMPLES];
        let mut discarded = [0.0; RNNOISE_FRAME_SAMPLES];
        let _ = warming_state.process_frame(&mut discarded, &silence);

        Ok(Box::new(RnnoiseCandidateDenoiser {
            descriptor: self.descriptor,
            model: self.model.clone(),
            state: DenoiseState::from_model(self.model.clone()),
            model_input: [0.0; RNNOISE_FRAME_SAMPLES],
            model_output: [0.0; RNNOISE_FRAME_SAMPLES],
        }))
    }
}

struct RnnoiseDenoiser {
    descriptor: ModelDescriptor,
    state: Box<DenoiseState<'static>>,
    model_input: [f32; RNNOISE_FRAME_SAMPLES],
    model_output: [f32; RNNOISE_FRAME_SAMPLES],
}

struct RnnoiseCandidateDenoiser {
    descriptor: ModelDescriptor,
    model: RnnModel,
    state: Box<DenoiseState<'static>>,
    model_input: [f32; RNNOISE_FRAME_SAMPLES],
    model_output: [f32; RNNOISE_FRAME_SAMPLES],
}

impl Denoiser for RnnoiseDenoiser {
    fn descriptor(&self) -> &ModelDescriptor {
        &self.descriptor
    }

    fn reset(&mut self) {
        // DenoiseState has no in-place reset. The shared contract requires this
        // method to run only while processing is deactivated.
        self.state = DenoiseState::new();
        self.model_input.fill(0.0);
        self.model_output.fill(0.0);
    }

    fn process_frame(
        &mut self,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<FrameStats, ProcessError> {
        prepare_process_frame(&self.descriptor, input, output)?;

        for (source, destination) in input.iter().zip(self.model_input.iter_mut()) {
            *destination = normalized_to_model_scale(*source);
        }

        let vad_probability = self
            .state
            .process_frame(&mut self.model_output, &self.model_input);
        let stats = FrameStats::new(vad_probability)?;
        finalize_process_output(&mut self.model_output, stats)?;

        for (source, destination) in self.model_output.iter().zip(output.iter_mut()) {
            *destination = model_to_normalized_scale(*source);
        }
        finalize_process_output(output, stats)
    }
}

impl Denoiser for RnnoiseCandidateDenoiser {
    fn descriptor(&self) -> &ModelDescriptor {
        &self.descriptor
    }

    fn reset(&mut self) {
        self.state = DenoiseState::from_model(self.model.clone());
        self.model_input.fill(0.0);
        self.model_output.fill(0.0);
    }

    fn process_frame(
        &mut self,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<FrameStats, ProcessError> {
        prepare_process_frame(&self.descriptor, input, output)?;
        for (source, destination) in input.iter().zip(self.model_input.iter_mut()) {
            *destination = normalized_to_model_scale(*source);
        }
        let vad_probability = self
            .state
            .process_frame(&mut self.model_output, &self.model_input);
        let stats = FrameStats::new(vad_probability)?;
        finalize_process_output(&mut self.model_output, stats)?;
        for (source, destination) in self.model_output.iter().zip(output.iter_mut()) {
            *destination = model_to_normalized_scale(*source);
        }
        finalize_process_output(output, stats)
    }
}

fn normalized_to_model_scale(sample: f32) -> f32 {
    let sample = sample.clamp(-1.0, 1.0);
    if sample.is_sign_negative() {
        sample * MODEL_INPUT_NEGATIVE_SCALE
    } else {
        sample * MODEL_INPUT_POSITIVE_SCALE
    }
}

fn model_to_normalized_scale(sample: f32) -> f32 {
    let normalized = if sample.is_sign_negative() {
        sample / MODEL_INPUT_NEGATIVE_SCALE
    } else {
        sample / MODEL_INPUT_POSITIVE_SCALE
    };
    normalized.clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_precision_loss, clippy::float_cmp)]

    use std::error::Error;

    use nnnoiseless::DenoiseState;
    use noire_model::{DenoiserFactory, ProcessError};

    use super::{
        DEFAULT_WEIGHTS_SHA256, MODEL_VERSION, QUALITY_V1_WEIGHTS_SHA256, RNNOISE_DELAY_SAMPLES,
        RNNOISE_FRAME_SAMPLES, RNNOISE_SAMPLE_RATE_HZ, RnnoiseCandidateFactory, RnnoiseFactory,
        model_to_normalized_scale, normalized_to_model_scale,
    };

    #[test]
    fn malformed_candidate_weights_are_rejected_before_activation() {
        assert!(RnnoiseCandidateFactory::from_bytes(&[]).is_err());
        assert!(RnnoiseCandidateFactory::from_bytes(&[0; 128]).is_err());
    }

    #[test]
    fn quality_v1_is_qualified_and_remains_distinct_from_the_default() -> Result<(), Box<dyn Error>>
    {
        let factory = RnnoiseCandidateFactory::quality_v1()?;
        let descriptor = factory.descriptor();
        assert_eq!(descriptor.id(), "org.noire.experimental.rnnoise-quality-v1");
        assert_eq!(descriptor.version(), "voicebank-quality-v1-2f0958c50378");
        assert_eq!(descriptor.license(), "BSD-3-Clause AND CC-BY-4.0");
        assert_eq!(QUALITY_V1_WEIGHTS_SHA256.len(), 64);
        let mut model = factory.create()?;
        let mut output = [0.0; RNNOISE_FRAME_SAMPLES];
        let stats = model.process_frame(&deterministic_signal(), &mut output)?;
        assert!(output.iter().all(|sample| sample.is_finite()));
        assert!((0.0..=1.0).contains(&stats.vad_probability()));
        Ok(())
    }

    #[test]
    fn descriptor_pins_default_model_format_delay_and_provenance() -> Result<(), Box<dyn Error>> {
        let factory = RnnoiseFactory::new()?;
        let descriptor = factory.descriptor();
        assert_eq!(descriptor.id(), "org.rnnoise.nnnoiseless.default");
        assert_eq!(descriptor.version(), MODEL_VERSION);
        assert_eq!(descriptor.license(), "BSD-3-Clause");
        assert_eq!(descriptor.sample_rate_hz(), RNNOISE_SAMPLE_RATE_HZ);
        assert_eq!(descriptor.channels(), 1);
        assert_eq!(descriptor.frame_samples(), RNNOISE_FRAME_SAMPLES);
        assert_eq!(descriptor.hop_samples(), RNNOISE_FRAME_SAMPLES);
        assert_eq!(descriptor.lookahead_samples(), 0);
        assert_eq!(descriptor.delay_samples(), RNNOISE_DELAY_SAMPLES);
        assert_eq!(RNNOISE_SAMPLE_RATE_HZ, noire_dsp::SAMPLE_RATE_HZ);
        assert_eq!(RNNOISE_FRAME_SAMPLES, 480);
        assert_eq!(
            usize::from(descriptor.channels()),
            noire_dsp::CANONICAL_CHANNELS
        );
        assert_eq!(DEFAULT_WEIGHTS_SHA256.len(), 64);
        Ok(())
    }

    #[test]
    fn normalized_scaling_honors_signed_sixteen_bit_endpoints() {
        assert_eq!(normalized_to_model_scale(-2.0), -32_768.0);
        assert_eq!(normalized_to_model_scale(-1.0), -32_768.0);
        assert_eq!(normalized_to_model_scale(-0.5), -16_384.0);
        assert_eq!(normalized_to_model_scale(0.0), 0.0);
        assert_eq!(normalized_to_model_scale(0.5), 16_383.5);
        assert_eq!(normalized_to_model_scale(1.0), 32_767.0);
        assert_eq!(normalized_to_model_scale(2.0), 32_767.0);

        assert_eq!(model_to_normalized_scale(-32_768.0), -1.0);
        assert_eq!(model_to_normalized_scale(32_767.0), 1.0);
        assert_eq!(model_to_normalized_scale(-65_536.0), -1.0);
        assert_eq!(model_to_normalized_scale(65_534.0), 1.0);
    }

    #[test]
    fn malformed_and_non_finite_frames_fail_closed() -> Result<(), Box<dyn Error>> {
        let factory = RnnoiseFactory::new()?;
        let mut model = factory.create()?;
        let mut output = [1.0; RNNOISE_FRAME_SAMPLES];
        let short = model.process_frame(&[0.0; RNNOISE_FRAME_SAMPLES - 1], &mut output);
        assert_eq!(short, Err(ProcessError::InputFrameLength));
        assert_eq!(output, [0.0; RNNOISE_FRAME_SAMPLES]);

        output.fill(1.0);
        let mut invalid = [0.0; RNNOISE_FRAME_SAMPLES];
        invalid[100] = f32::NAN;
        let non_finite = model.process_frame(&invalid, &mut output);
        assert_eq!(non_finite, Err(ProcessError::NonFiniteInput));
        assert_eq!(output, [0.0; RNNOISE_FRAME_SAMPLES]);
        Ok(())
    }

    #[test]
    fn silence_stays_finite_and_silent_with_zero_vad() -> Result<(), Box<dyn Error>> {
        let factory = RnnoiseFactory::new()?;
        let mut model = factory.create()?;
        let mut output = [1.0; RNNOISE_FRAME_SAMPLES];
        let stats = model.process_frame(&[0.0; RNNOISE_FRAME_SAMPLES], &mut output)?;
        assert_eq!(output, [0.0; RNNOISE_FRAME_SAMPLES]);
        assert_eq!(stats.vad_probability(), 0.0);
        Ok(())
    }

    #[test]
    fn deterministic_signal_produces_bounded_finite_output_and_vad() -> Result<(), Box<dyn Error>> {
        let factory = RnnoiseFactory::new()?;
        let mut model = factory.create()?;
        let input = deterministic_signal();
        let mut output = [0.0; RNNOISE_FRAME_SAMPLES];
        let stats = model.process_frame(&input, &mut output)?;
        assert!(output.iter().all(|sample| sample.is_finite()));
        assert!(output.iter().all(|sample| (-1.0..=1.0).contains(sample)));
        assert!((0.0..=1.0).contains(&stats.vad_probability()));
        Ok(())
    }

    #[test]
    fn adapter_output_and_vad_match_direct_default_model() -> Result<(), Box<dyn Error>> {
        let factory = RnnoiseFactory::new()?;
        let mut adapter = factory.create()?;
        let mut direct = DenoiseState::new();
        let mut adapter_output = [0.0; RNNOISE_FRAME_SAMPLES];
        let mut direct_input = [0.0; RNNOISE_FRAME_SAMPLES];
        let mut direct_output = [0.0; RNNOISE_FRAME_SAMPLES];

        for frame_index in 0..8 {
            let input = deterministic_frame(frame_index);
            let stats = adapter.process_frame(&input, &mut adapter_output)?;
            for (source, destination) in input.iter().zip(direct_input.iter_mut()) {
                *destination = normalized_to_model_scale(*source);
            }
            let direct_vad = direct.process_frame(&mut direct_output, &direct_input);
            assert_eq!(stats.vad_probability(), direct_vad);
            for (adapter_sample, direct_sample) in adapter_output.iter().zip(direct_output.iter()) {
                assert_eq!(*adapter_sample, model_to_normalized_scale(*direct_sample));
            }
        }
        Ok(())
    }

    #[test]
    fn reset_reproduces_clean_default_state() -> Result<(), Box<dyn Error>> {
        let factory = RnnoiseFactory::new()?;
        let mut model = factory.create()?;
        let input = deterministic_signal();
        let mut first = [0.0; RNNOISE_FRAME_SAMPLES];
        let mut changed = [0.0; RNNOISE_FRAME_SAMPLES];
        let mut after_reset = [0.0; RNNOISE_FRAME_SAMPLES];
        let first_stats = model.process_frame(&input, &mut first)?;
        model.process_frame(&input, &mut changed)?;
        model.reset();
        let reset_stats = model.process_frame(&input, &mut after_reset)?;

        assert_ne!(changed, first);
        assert_eq!(after_reset, first);
        assert_eq!(reset_stats, first_stats);
        Ok(())
    }

    #[test]
    fn impulse_response_confirms_one_frame_startup_delay() -> Result<(), Box<dyn Error>> {
        let factory = RnnoiseFactory::new()?;
        let mut model = factory.create()?;
        let mut impulse = [0.0; RNNOISE_FRAME_SAMPLES];
        impulse[240] = 0.75;
        let mut first = [0.0; RNNOISE_FRAME_SAMPLES];
        let mut second = [0.0; RNNOISE_FRAME_SAMPLES];
        let mut third = [0.0; RNNOISE_FRAME_SAMPLES];
        model.process_frame(&impulse, &mut first)?;
        model.process_frame(&[0.0; RNNOISE_FRAME_SAMPLES], &mut second)?;
        model.process_frame(&[0.0; RNNOISE_FRAME_SAMPLES], &mut third)?;
        let first_energy = energy(&first);
        let second_energy = energy(&second);
        let third_energy = energy(&third);
        assert!(second_energy > first_energy * 1_000.0);
        assert!(second_energy > third_energy * 100.0);
        let delayed_peak = RNNOISE_FRAME_SAMPLES + peak_index(&second);
        assert_eq!(delayed_peak - 240, RNNOISE_DELAY_SAMPLES);
        Ok(())
    }

    fn energy(frame: &[f32]) -> f32 {
        frame.iter().map(|sample| sample * sample).sum()
    }

    fn peak_index(frame: &[f32]) -> usize {
        frame
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.abs().total_cmp(&right.abs()))
            .map_or(0, |(index, _)| index)
    }

    fn deterministic_signal() -> [f32; RNNOISE_FRAME_SAMPLES] {
        deterministic_frame(0)
    }

    fn deterministic_frame(frame_index: usize) -> [f32; RNNOISE_FRAME_SAMPLES] {
        let mut signal = [0.0; RNNOISE_FRAME_SAMPLES];
        let mut phase = frame_index as f32 * 0.37;
        let phase_step = 2.0 * core::f32::consts::PI * 440.0 / 48_000.0;
        for (sample_index, sample) in signal.iter_mut().enumerate() {
            let pseudo_noise = ((frame_index * RNNOISE_FRAME_SAMPLES + sample_index)
                .wrapping_mul(1_103_515_245)
                .wrapping_add(12_345)
                >> 16)
                & 0x7fff;
            let pseudo_noise = pseudo_noise as f32 / 32_767.0 * 0.04 - 0.02;
            *sample = phase.sin() * 0.25 + pseudo_noise;
            phase += phase_step;
        }
        signal
    }
}
