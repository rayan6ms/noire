//! Bounded click-safe control ramps and wet/dry mixing.

use crate::{MIN_STRENGTH_RAMP_SAMPLES, SanitizeReport, sanitize_sample};

const SPEECH_VAD_LOW: f32 = 0.20;
const SPEECH_VAD_HIGH: f32 = 0.80;
const SPEECH_STRENGTH_RATIO: f32 = 0.55 / 0.70;
const SPEECH_ATTACK_SECONDS: f32 = 0.010;
const NOISE_RELEASE_SECONDS: f32 = 0.100;
const ADAPTIVE_SPEECH_ATTACK_SECONDS: f32 = 0.005;
const ADAPTIVE_NOISE_RELEASE_SECONDS: f32 = 0.150;
const SPEECH_ENTER_PROBABILITY: f32 = 0.65;
const SPEECH_EXIT_PROBABILITY: f32 = 0.25;
const NON_SPEECH_CONFIRM_FRAMES: u8 = 3;

const PERSONALIZED_ENTER_PROBABILITY: f32 = 0.80;
const PERSONALIZED_EXIT_PROBABILITY: f32 = 0.55;
const PERSONALIZED_EXIT_FRAMES: u8 = 3;
const PERSONALIZED_ENGAGE_SECONDS: f32 = 0.080;
const GENERIC_FALLBACK_SECONDS: f32 = 0.020;

/// A strength ramp that always takes at least 20 ms for a nontrivial change.
#[derive(Clone, Copy, Debug)]
pub struct StrengthRamp {
    current: f32,
    target: f32,
    step: f32,
    remaining: u32,
}

impl StrengthRamp {
    /// Creates a settled ramp, clamping the initial strength to `[0, 1]`.
    #[must_use]
    pub fn new(initial: f32) -> Self {
        let initial = finite_unit(initial);
        Self {
            current: initial,
            target: initial,
            step: 0.0,
            remaining: 0,
        }
    }

    /// Starts a transition, enforcing the 20 ms minimum duration.
    pub fn set_target(&mut self, target: f32, requested_samples: u32) {
        let target = finite_unit(target);
        if target.to_bits() == self.current.to_bits() {
            self.target = target;
            self.step = 0.0;
            self.remaining = 0;
            return;
        }

        let duration = requested_samples.max(MIN_STRENGTH_RAMP_SAMPLES);
        self.target = target;
        self.step = (target - self.current) / duration_as_f32(duration);
        self.remaining = duration;
    }

    /// Returns the next strength value, landing exactly on the target.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> f32 {
        if self.remaining == 0 {
            return self.current;
        }
        self.remaining -= 1;
        if self.remaining == 0 {
            self.current = self.target;
        } else {
            self.current += self.step;
        }
        self.current
    }

    /// Returns the most recently produced strength.
    #[must_use]
    pub const fn current(&self) -> f32 {
        self.current
    }

    /// Returns the requested target.
    #[must_use]
    pub const fn target(&self) -> f32 {
        self.target
    }

    /// Returns samples remaining in the transition.
    #[must_use]
    pub const fn remaining(&self) -> u32 {
        self.remaining
    }
}

/// Smoothly reduces wet strength while the model reports speech.
///
/// `RNNoise` remains at the requested strength for noise-only frames. During
/// confident speech, the effective strength is reduced by at most about 21%
/// to preserve voice detail. The asymmetric envelope reacts quickly to speech
/// and returns slowly enough to avoid audible pumping.
#[derive(Clone, Copy, Debug)]
pub struct SpeechPreservingStrength {
    factor: f32,
    target_factor: f32,
    attack_coefficient: f32,
    release_coefficient: f32,
}

impl Default for SpeechPreservingStrength {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeechPreservingStrength {
    /// Creates a controller settled at the unmodified requested strength.
    #[must_use]
    pub fn new() -> Self {
        Self {
            factor: 1.0,
            target_factor: 1.0,
            attack_coefficient: smoothing_coefficient(SPEECH_ATTACK_SECONDS),
            release_coefficient: smoothing_coefficient(NOISE_RELEASE_SECONDS),
        }
    }

    /// Sets the frame-level VAD probability used by subsequent samples.
    pub fn begin_frame(&mut self, vad_probability: f32) {
        let probability = finite_unit(vad_probability);
        let position =
            ((probability - SPEECH_VAD_LOW) / (SPEECH_VAD_HIGH - SPEECH_VAD_LOW)).clamp(0.0, 1.0);
        let speech = position * position * (3.0 - 2.0 * position);
        self.target_factor = 1.0 + (SPEECH_STRENGTH_RATIO - 1.0) * speech;
    }

    /// Returns the next effective strength for a requested base strength.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self, base_strength: f32) -> f32 {
        let base_strength = finite_unit(base_strength);
        let coefficient = if self.target_factor < self.factor {
            self.attack_coefficient
        } else {
            self.release_coefficient
        };
        self.factor = self.target_factor + coefficient * (self.factor - self.target_factor);
        if base_strength <= 0.0 || base_strength >= 1.0 {
            base_strength
        } else {
            base_strength * self.factor
        }
    }

    /// Clears the speech envelope without changing its policy.
    pub const fn reset(&mut self) {
        self.factor = 1.0;
        self.target_factor = 1.0;
    }
}

/// Opt-in next-generation suppression controller with pause hysteresis.
///
/// Unlike [`SpeechPreservingStrength`], which remains the frozen production
/// behavior, this prototype backs off within 5 ms on confident speech and
/// takes 150 ms to restore pause attenuation after three confirmed non-speech
/// frames. Keeping the policies as separate types makes baseline A/B runs
/// explicit and prevents an unevaluated controller from silently shipping.
#[derive(Clone, Copy, Debug)]
pub struct AdaptiveSuppressionController {
    factor: f32,
    target_factor: f32,
    attack_coefficient: f32,
    release_coefficient: f32,
    speech_active: bool,
    non_speech_frames: u8,
}

impl Default for AdaptiveSuppressionController {
    fn default() -> Self {
        Self::new()
    }
}

impl AdaptiveSuppressionController {
    /// Creates a controller settled at full requested suppression.
    #[must_use]
    pub fn new() -> Self {
        Self {
            factor: 1.0,
            target_factor: 1.0,
            attack_coefficient: smoothing_coefficient(ADAPTIVE_SPEECH_ATTACK_SECONDS),
            release_coefficient: smoothing_coefficient(ADAPTIVE_NOISE_RELEASE_SECONDS),
            speech_active: false,
            non_speech_frames: 0,
        }
    }

    /// Updates the delay-aligned frame-level speech probability.
    pub fn begin_frame(&mut self, vad_probability: f32) {
        let probability = finite_unit(vad_probability);
        if probability >= SPEECH_ENTER_PROBABILITY {
            self.speech_active = true;
            self.non_speech_frames = 0;
        } else if self.speech_active && probability <= SPEECH_EXIT_PROBABILITY {
            self.non_speech_frames = self.non_speech_frames.saturating_add(1);
            if self.non_speech_frames >= NON_SPEECH_CONFIRM_FRAMES {
                self.speech_active = false;
                self.non_speech_frames = 0;
            }
        } else {
            self.non_speech_frames = 0;
        }

        let position =
            ((probability - SPEECH_VAD_LOW) / (SPEECH_VAD_HIGH - SPEECH_VAD_LOW)).clamp(0.0, 1.0);
        let mut speech = position * position * (3.0 - 2.0 * position);
        if self.speech_active {
            speech = speech.max(0.25);
        }
        self.target_factor = 1.0 + (SPEECH_STRENGTH_RATIO - 1.0) * speech;
    }

    /// Returns the next smoothly adjusted suppression strength.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self, base_strength: f32) -> f32 {
        let base_strength = finite_unit(base_strength);
        let coefficient = if self.target_factor < self.factor {
            self.attack_coefficient
        } else {
            self.release_coefficient
        };
        self.factor = self.target_factor + coefficient * (self.factor - self.target_factor);
        if base_strength <= 0.0 || base_strength >= 1.0 {
            base_strength
        } else {
            base_strength * self.factor
        }
    }

    /// Clears the adaptive state without changing its policy.
    pub const fn reset(&mut self) {
        self.factor = 1.0;
        self.target_factor = 1.0;
        self.speech_active = false;
        self.non_speech_frames = 0;
    }
}

/// Confidence-weighted crossfade between generic and personalized processing.
///
/// Invalid or falling confidence always moves toward the generic path. Three
/// consecutive low-confidence frames are required to leave personalized mode,
/// and both directions remain sample-smoothed. The type contains no model or
/// enrollment logic; it is a safe transition primitive for a future optional
/// target-speaker path.
#[derive(Clone, Copy, Debug)]
pub struct ConfidenceWeightedBlend {
    mix: f32,
    target_mix: f32,
    personalized_active: bool,
    low_confidence_frames: u8,
    engage_coefficient: f32,
    fallback_coefficient: f32,
}

impl Default for ConfidenceWeightedBlend {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfidenceWeightedBlend {
    /// Creates a blend settled on generic enhancement.
    #[must_use]
    pub fn new() -> Self {
        Self {
            mix: 0.0,
            target_mix: 0.0,
            personalized_active: false,
            low_confidence_frames: 0,
            engage_coefficient: smoothing_coefficient(PERSONALIZED_ENGAGE_SECONDS),
            fallback_coefficient: smoothing_coefficient(GENERIC_FALLBACK_SECONDS),
        }
    }

    /// Updates the target-speaker confidence for subsequent samples.
    pub fn begin_frame(&mut self, confidence: f32) {
        let confidence = if confidence.is_finite() {
            confidence.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if confidence >= PERSONALIZED_ENTER_PROBABILITY {
            self.personalized_active = true;
            self.low_confidence_frames = 0;
        } else if self.personalized_active && confidence <= PERSONALIZED_EXIT_PROBABILITY {
            self.low_confidence_frames = self.low_confidence_frames.saturating_add(1);
            if self.low_confidence_frames >= PERSONALIZED_EXIT_FRAMES {
                self.personalized_active = false;
                self.low_confidence_frames = 0;
            }
        } else {
            self.low_confidence_frames = 0;
        }

        self.target_mix = if self.personalized_active {
            smoothstep(
                PERSONALIZED_EXIT_PROBABILITY,
                PERSONALIZED_ENTER_PROBABILITY,
                confidence,
            )
        } else {
            0.0
        };
    }

    /// Returns the next personalized-path contribution in `[0, 1]`.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> f32 {
        let coefficient = if self.target_mix > self.mix {
            self.engage_coefficient
        } else {
            self.fallback_coefficient
        };
        self.mix = self.target_mix + coefficient * (self.mix - self.target_mix);
        self.mix
    }

    /// Clears hysteresis and returns immediately to generic processing.
    pub const fn reset(&mut self) {
        self.mix = 0.0;
        self.target_mix = 0.0;
        self.personalized_active = false;
        self.low_confidence_frames = 0;
    }
}

/// Counts sanitization and transparent hard-ceiling events during mixing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MixReport {
    /// Invalid or denormal samples replaced with zero.
    pub sanitized: SanitizeReport,
    /// Finite mixed samples constrained to the canonical range.
    pub hard_ceiling: u64,
}

/// A stateless linear wet/dry mixer for latency-aligned, correlated signals.
///
/// A linear blend is intentionally used here: dry speech and denoised speech
/// are strongly correlated, so an equal-power crossfade would add gain at
/// intermediate strengths and could clip otherwise valid samples.
#[derive(Clone, Copy, Debug, Default)]
pub struct LinearMixer;

impl LinearMixer {
    /// Mixes one latency-aligned dry/wet sample at a strength in `[0, 1]`.
    ///
    /// Strength zero returns dry exactly and strength one returns wet exactly.
    #[must_use]
    pub fn mix(dry: f32, wet: f32, strength: f32, report: &mut MixReport) -> f32 {
        let dry = sanitize_sample(dry, &mut report.sanitized);
        let wet = sanitize_sample(wet, &mut report.sanitized);
        let strength = finite_unit(strength);

        let mixed = if strength <= 0.0 {
            dry
        } else if strength >= 1.0 {
            wet
        } else {
            dry * (1.0 - strength) + wet * strength
        };
        let mixed = sanitize_sample(mixed, &mut report.sanitized);
        let limited = mixed.clamp(-1.0, 1.0);
        if !(-1.0..=1.0).contains(&mixed) {
            report.hard_ceiling += 1;
        }
        limited
    }
}

/// Compatibility alias retained for downstream users of the phase-2 API.
#[doc(hidden)]
pub type EqualPowerMixer = LinearMixer;

fn finite_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[allow(clippy::cast_precision_loss)]
fn duration_as_f32(duration: u32) -> f32 {
    duration as f32
}

fn smoothing_coefficient(seconds: f32) -> f32 {
    debug_assert_eq!(crate::SAMPLE_RATE_HZ, 48_000);
    (-1.0 / (seconds * 48_000.0)).exp()
}

fn smoothstep(low: f32, high: f32, value: f32) -> f32 {
    let position = ((value - low) / (high - low)).clamp(0.0, 1.0);
    position * position * (3.0 - 2.0 * position)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_precision_loss, clippy::float_cmp)]

    use super::{
        AdaptiveSuppressionController, ConfidenceWeightedBlend, LinearMixer, MixReport,
        SpeechPreservingStrength, StrengthRamp,
    };
    use crate::{DryDelay, MIN_STRENGTH_RAMP_SAMPLES, MODEL_FRAME_SAMPLES, SAMPLE_RATE_HZ};

    #[test]
    fn strength_endpoints_are_exact() {
        let mut report = MixReport::default();
        assert_eq!(LinearMixer::mix(0.25, -0.75, 0.0, &mut report), 0.25);
        assert_eq!(LinearMixer::mix(0.25, -0.75, 1.0, &mut report), -0.75);
    }

    #[test]
    fn strength_transition_enforces_minimum_and_lands_exactly() {
        let mut ramp = StrengthRamp::new(0.0);
        ramp.set_target(1.0, 1);
        assert_eq!(ramp.remaining(), MIN_STRENGTH_RAMP_SAMPLES);
        for _ in 0..MIN_STRENGTH_RAMP_SAMPLES {
            ramp.next();
        }
        assert_eq!(ramp.current(), 1.0);
        assert_eq!(ramp.remaining(), 0);
    }

    #[test]
    fn mixer_sanitizes_and_limits_only_out_of_range_results() {
        let mut report = MixReport::default();
        assert_eq!(LinearMixer::mix(f32::NAN, 0.5, 0.0, &mut report), 0.0);
        assert_eq!(LinearMixer::mix(1.0, 1.0, 0.5, &mut report), 1.0);
        assert_eq!(report.sanitized.non_finite, 1);
        assert_eq!(report.hard_ceiling, 0);
    }

    #[test]
    fn correlated_inputs_do_not_gain_or_hit_the_ceiling() {
        let mut report = MixReport::default();
        for strength in [0.25, 0.5, 0.75] {
            assert_eq!(LinearMixer::mix(0.9, 0.9, strength, &mut report), 0.9);
        }
        assert_eq!(report.hard_ceiling, 0);
    }

    #[test]
    fn confident_speech_converges_to_the_audited_default_strength() {
        let mut controller = SpeechPreservingStrength::new();
        controller.begin_frame(1.0);
        let mut effective = 0.0;
        for _ in 0..48_000 {
            effective = controller.next(0.70);
        }
        assert!((effective - 0.55).abs() < 1.0e-4);
    }

    #[test]
    fn speech_controller_is_bounded_and_handles_invalid_inputs() {
        let mut controller = SpeechPreservingStrength::new();
        controller.begin_frame(f32::NAN);
        assert!((controller.next(0.70) - 0.70).abs() < 1.0e-6);
        controller.begin_frame(1.0);
        for _ in 0..4_800 {
            let value = controller.next(f32::INFINITY);
            assert_eq!(value, 0.0);
        }
        assert_eq!(controller.next(1.0), 1.0);
        controller.reset();
        assert!((controller.next(1.0) - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn speech_backoff_is_fast_and_pause_recovery_is_hysteretic() {
        let mut controller = AdaptiveSuppressionController::new();
        controller.begin_frame(1.0);
        let after_five_ms = (0..240).fold(0.70, |_, _| controller.next(0.70));
        assert!(after_five_ms < 0.61);

        for _ in 0..2 {
            controller.begin_frame(0.0);
            for _ in 0..MODEL_FRAME_SAMPLES {
                controller.next(0.70);
            }
        }
        let held = controller.next(0.70);
        assert!(held < 0.64);

        controller.begin_frame(0.0);
        let recovered = (0..7_200).fold(held, |_, _| controller.next(0.70));
        assert!(recovered > held);
        assert!(recovered < 0.70);
    }

    #[test]
    fn personalized_blend_uses_hysteresis_and_smooth_generic_fallback() {
        let mut blend = ConfidenceWeightedBlend::new();
        blend.begin_frame(1.0);
        let engaged = (0..4_800).fold(0.0, |_, _| blend.next());
        assert!(engaged > 0.70 && engaged < 1.0);

        for _ in 0..2 {
            blend.begin_frame(0.0);
            for _ in 0..MODEL_FRAME_SAMPLES {
                blend.next();
            }
        }
        let held = blend.next();
        assert!(held > 0.0);

        blend.begin_frame(0.0);
        let first_fallback = blend.next();
        assert!(first_fallback > 0.0 && first_fallback < held);
        let generic = (0..4_800).fold(first_fallback, |_, _| blend.next());
        assert!(generic < 0.01);
        blend.reset();
        assert_eq!(blend.next(), 0.0);
    }

    #[test]
    fn latency_matched_zero_strength_bypass_preserves_tone()
    -> Result<(), Box<dyn std::error::Error>> {
        const DELAY: usize = 480;
        const SAMPLES: usize = 4_800;
        let mut input = vec![0.0; SAMPLES];
        let phase_step = 2.0 * core::f32::consts::PI * 1_000.0 / SAMPLE_RATE_HZ as f32;
        let mut phase = 0.0_f32;
        for sample in &mut input {
            *sample = phase.sin() * 0.5;
            phase += phase_step;
        }

        let mut delay = DryDelay::new(DELAY)?;
        let mut output = vec![0.0; SAMPLES];
        let mut offset = 0;
        for size in [64, 128, 256, 480, 512].into_iter().cycle() {
            if offset == SAMPLES {
                break;
            }
            let end = (offset + size).min(SAMPLES);
            let mut dry = [0.0; 512];
            delay.process(&input[offset..end], &mut dry[..end - offset])?;
            let mut report = MixReport::default();
            for (dry, destination) in dry[..end - offset]
                .iter()
                .zip(output[offset..end].iter_mut())
            {
                *destination = LinearMixer::mix(*dry, -*dry, 0.0, &mut report);
            }
            assert_eq!(report.hard_ceiling, 0);
            offset = end;
        }

        assert!(output.iter().all(|sample| sample.is_finite()));
        assert!(output.iter().all(|sample| sample.abs() <= 1.0));
        for (actual, expected) in output[DELAY..].iter().zip(input[..SAMPLES - DELAY].iter()) {
            assert!((*actual - *expected).abs() <= 1.0e-6);
        }
        let input_energy: f32 = input[..SAMPLES - DELAY]
            .iter()
            .map(|sample| sample * sample)
            .sum();
        let output_energy: f32 = output[DELAY..].iter().map(|sample| sample * sample).sum();
        let gain_db = 10.0 * (output_energy / input_energy).log10();
        assert!(gain_db.abs() <= 0.1);
        Ok(())
    }
}
