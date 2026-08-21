//! Conservative causal reduction of late room-reverberation tails.

use crate::{MODEL_FRAME_SAMPLES, SAMPLE_RATE_HZ, SanitizeReport, sanitize_sample};

const LATE_DELAY_SAMPLES: usize = 1_920;
const MAX_PREDICTION: f32 = 0.72;
const ESTIMATOR_SECONDS: f32 = 0.160;
const SPEECH_BACKOFF_SECONDS: f32 = 0.005;
const NON_SPEECH_ENGAGE_SECONDS: f32 = 0.120;
const MIN_CORRELATION: f32 = 0.12;
const FULL_CORRELATION: f32 = 0.42;

/// Configuration for the conservative late-tail reducer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LateReverbConfig {
    /// Maximum wet contribution of the estimated late-tail cancellation.
    pub strength: f32,
}

impl Default for LateReverbConfig {
    fn default() -> Self {
        Self { strength: 0.35 }
    }
}

/// Diagnostics from one exact-frame dereverberation call.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LateReverbReport {
    /// Current bounded late-path prediction coefficient.
    pub prediction: f32,
    /// Current normalized evidence that a correlated late tail is present.
    pub evidence: f32,
    /// Current smoothed cancellation mix in `[0, 1]`.
    pub mix: f32,
    /// Sanitization performed at the processor boundary.
    pub sanitized: SanitizeReport,
    /// Samples constrained to the normalized output range.
    pub hard_ceiling: u64,
}

/// A fixed-memory, zero-lookahead late-reverberation reducer.
///
/// The processor estimates only energy correlated with audio at least 40 ms in
/// the past. That lower bound deliberately protects the direct path and useful
/// early reflections. Cancellation disengages quickly when speech is present
/// and engages slowly only during confident non-speech with positive late-path
/// evidence. It is intentionally conservative: a future learned multi-frame
/// core can replace it without changing the real-time boundary.
#[derive(Clone, Debug)]
pub struct LateReverbReducer {
    history: Box<[f32; LATE_DELAY_SAMPLES]>,
    position: usize,
    correlation: f32,
    reference_power: f32,
    prediction: f32,
    mix: f32,
    config: LateReverbConfig,
    estimator_coefficient: f32,
    speech_backoff_coefficient: f32,
    non_speech_engage_coefficient: f32,
}

impl LateReverbReducer {
    /// Allocates and initializes history outside the audio callback.
    #[must_use]
    pub fn new(config: LateReverbConfig) -> Self {
        Self {
            history: Box::new([0.0; LATE_DELAY_SAMPLES]),
            position: 0,
            correlation: 0.0,
            reference_power: 0.0,
            prediction: 0.0,
            mix: 0.0,
            config: LateReverbConfig {
                strength: finite_unit(config.strength),
            },
            estimator_coefficient: smoothing_coefficient(ESTIMATOR_SECONDS),
            speech_backoff_coefficient: smoothing_coefficient(SPEECH_BACKOFF_SECONDS),
            non_speech_engage_coefficient: smoothing_coefficient(NON_SPEECH_ENGAGE_SECONDS),
        }
    }

    /// Returns the first delay considered late reverberation.
    #[must_use]
    pub const fn late_delay_samples(&self) -> usize {
        LATE_DELAY_SAMPLES
    }

    /// Clears signal and estimator history without allocating.
    pub fn reset(&mut self) {
        self.history.fill(0.0);
        self.position = 0;
        self.correlation = 0.0;
        self.reference_power = 0.0;
        self.prediction = 0.0;
        self.mix = 0.0;
    }

    /// Processes one exact model frame without allocating or adding latency.
    ///
    /// `speech_probability` must describe the audio in `input`, including any
    /// upstream model delay. Invalid probabilities are treated as speech so the
    /// processor fails toward preservation rather than stronger cancellation.
    pub fn process_frame<const FRAME_SAMPLES: usize>(
        &mut self,
        input: &[f32; FRAME_SAMPLES],
        output: &mut [f32; FRAME_SAMPLES],
        speech_probability: f32,
    ) -> LateReverbReport {
        let speech = if speech_probability.is_finite() {
            smoothstep(0.18, 0.72, speech_probability)
        } else {
            1.0
        };
        let non_speech = 1.0 - speech;
        let mut report = LateReverbReport::default();

        for (source, destination) in input.iter().zip(output.iter_mut()) {
            let current = sanitize_sample(*source, &mut report.sanitized);
            let delayed = self.history[self.position];
            self.history[self.position] = current;
            self.position += 1;
            if self.position == LATE_DELAY_SAMPLES {
                self.position = 0;
            }

            let update = (1.0 - self.estimator_coefficient) * non_speech;
            self.correlation += update * (current * delayed - self.correlation);
            self.reference_power += update * (delayed * delayed - self.reference_power);

            let estimate = if self.reference_power > 1.0e-9 {
                (self.correlation / self.reference_power).clamp(0.0, MAX_PREDICTION)
            } else {
                0.0
            };
            self.prediction += update * (estimate - self.prediction);

            let evidence = smoothstep(MIN_CORRELATION, FULL_CORRELATION, self.prediction);
            let target_mix = self.config.strength * non_speech * evidence;
            let coefficient = if target_mix < self.mix {
                self.speech_backoff_coefficient
            } else {
                self.non_speech_engage_coefficient
            };
            self.mix = target_mix + coefficient * (self.mix - target_mix);

            let predicted_tail = self.prediction * delayed;
            let requested_cancellation = self.mix * predicted_tail;
            // Constrain cancellation, rather than hard-limiting its result, so
            // this stage cannot create a new full-scale overrun when current
            // and delayed samples momentarily have opposite signs.
            let cancellation = requested_cancellation.clamp(current - 1.0, current + 1.0);
            let mixed = current - cancellation;
            let limited = mixed.clamp(-1.0, 1.0);
            report.hard_ceiling += u64::from(limited.to_bits() != mixed.to_bits());
            *destination = sanitize_sample(limited, &mut report.sanitized);
            report.evidence = evidence;
        }

        report.prediction = self.prediction;
        report.mix = self.mix;
        report
    }
}

fn finite_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn smoothing_coefficient(seconds: f32) -> f32 {
    debug_assert_eq!(SAMPLE_RATE_HZ, 48_000);
    (-1.0 / (seconds * 48_000.0)).exp()
}

fn smoothstep(low: f32, high: f32, value: f32) -> f32 {
    let position = ((value - low) / (high - low)).clamp(0.0, 1.0);
    position * position * (3.0 - 2.0 * position)
}

const _: () = assert!(LATE_DELAY_SAMPLES >= MODEL_FRAME_SAMPLES * 3);

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_precision_loss, clippy::float_cmp)]

    use super::{LateReverbConfig, LateReverbReducer};
    use crate::{MODEL_FRAME_SAMPLES, ModelFrame};

    #[test]
    fn clean_speech_is_sample_exact_while_speech_is_present() {
        let mut reducer = LateReverbReducer::new(LateReverbConfig::default());
        for frame_index in 0..24 {
            let input = signal_frame(frame_index);
            let mut output = [0.0; MODEL_FRAME_SAMPLES];
            let report = reducer.process_frame(&input, &mut output, 1.0);
            assert_eq!(output, input);
            assert_eq!(report.mix, 0.0);
            assert_eq!(report.hard_ceiling, 0);
        }
    }

    #[test]
    fn invalid_probability_fails_toward_preservation() {
        let mut reducer = LateReverbReducer::new(LateReverbConfig { strength: 1.0 });
        let input = signal_frame(0);
        let mut output = [0.0; MODEL_FRAME_SAMPLES];
        reducer.process_frame(&input, &mut output, f32::NAN);
        assert_eq!(output, input);
    }

    #[test]
    fn repeated_late_tail_is_reduced_without_clipping() {
        const FRAMES: usize = 80;
        const DELAY: usize = 1_920;
        let mut dry = vec![0.0_f32; FRAMES * MODEL_FRAME_SAMPLES];
        for burst in [0, 16_000, 28_000] {
            for offset in 0..2_400 {
                if let Some(sample) = dry.get_mut(burst + offset) {
                    let phase = offset as f32 * 0.071;
                    *sample = phase.sin() * 0.28;
                }
            }
        }
        let mut reverberant = vec![0.0_f32; dry.len()];
        for index in 0..reverberant.len() {
            let late = index
                .checked_sub(DELAY)
                .map_or(0.0, |past| reverberant[past] * 0.58);
            reverberant[index] = dry[index] + late;
        }

        let mut reducer = LateReverbReducer::new(LateReverbConfig { strength: 0.65 });
        let mut processed = vec![0.0_f32; reverberant.len()];
        for (frame_index, input) in reverberant.chunks_exact(MODEL_FRAME_SAMPLES).enumerate() {
            let mut frame: ModelFrame = [0.0; MODEL_FRAME_SAMPLES];
            frame.copy_from_slice(input);
            let mut output = [0.0; MODEL_FRAME_SAMPLES];
            let start = frame_index * MODEL_FRAME_SAMPLES;
            let speech = if dry[start..start + MODEL_FRAME_SAMPLES]
                .iter()
                .any(|sample| sample.abs() > 1.0e-5)
            {
                1.0
            } else {
                0.0
            };
            let report = reducer.process_frame(&frame, &mut output, speech);
            assert_eq!(report.hard_ceiling, 0);
            processed[start..start + MODEL_FRAME_SAMPLES].copy_from_slice(&output);
        }

        let tail = 34_000..36_000;
        let before = energy(&reverberant[tail.clone()]);
        let after = energy(&processed[tail]);
        assert!(
            after < before * 0.90,
            "late-tail energy {after} was not below {before}"
        );
    }

    #[test]
    fn full_scale_stress_cannot_create_clipping() {
        let mut reducer = LateReverbReducer::new(LateReverbConfig { strength: 1.0 });
        for frame_index in 0..200 {
            let mut input = [0.0; MODEL_FRAME_SAMPLES];
            for (sample_index, sample) in input.iter_mut().enumerate() {
                *sample = if (frame_index * MODEL_FRAME_SAMPLES + sample_index) % 97 < 49 {
                    1.0
                } else {
                    -1.0
                };
            }
            let mut output = [0.0; MODEL_FRAME_SAMPLES];
            let report = reducer.process_frame(&input, &mut output, 0.0);
            assert_eq!(report.hard_ceiling, 0);
            assert!(output.iter().all(|sample| (-1.0..=1.0).contains(sample)));
        }
    }

    fn signal_frame(frame_index: usize) -> ModelFrame {
        let mut frame = [0.0; MODEL_FRAME_SAMPLES];
        for (index, sample) in frame.iter_mut().enumerate() {
            let absolute = frame_index * MODEL_FRAME_SAMPLES + index;
            *sample = (absolute as f32 * 0.037).sin() * 0.25;
        }
        frame
    }

    fn energy(samples: &[f32]) -> f32 {
        samples.iter().map(|sample| sample * sample).sum()
    }
}
