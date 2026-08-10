//! Bounded fail-closed fades and transition click detection.

use core::fmt;

use crate::{
    CLICK_EXCESS_THRESHOLD, FAULT_RAMP_SAMPLES, MAX_CALLBACK_FRAMES, SanitizeReport,
    sanitize_sample,
};

const HALF_PI: f32 = core::f32::consts::FRAC_PI_2;

/// Current overflow/underflow transition state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FaultRampState {
    /// Valid processed audio passes at unity gain.
    #[default]
    Passing,
    /// Previously processed audio is fading to silence.
    FadingOut,
    /// Output is fail-closed silence while fresh startup data is unavailable.
    Silent,
    /// Fresh processed audio is fading back from silence.
    FadingIn,
}

/// A fault-ramp processing error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultRampError {
    /// Present input and output have different lengths.
    ShapeMismatch,
    /// The requested callback exceeds the fixed processing bound.
    QuantumTooLarge,
}

impl fmt::Display for FaultRampError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ShapeMismatch => "fault-ramp input and output lengths differ",
            Self::QuantumTooLarge => "fault-ramp callback exceeds the fixed boundary",
        })
    }
}

impl std::error::Error for FaultRampError {}

/// Counters and final state from one bounded fault-ramp call.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FaultRampReport {
    /// Samples synthesized because processed input was unavailable.
    pub missing_samples: u16,
    /// Invalid or subnormal values replaced with silence.
    pub sanitized: SanitizeReport,
    /// State after the final output sample.
    pub state: FaultRampState,
}

/// A fixed-duration equal-power gain ramp for fail-closed transitions.
///
/// Overflow handling calls [`Self::begin_fault`] while draining already-
/// processed audio. Underflow may pass `None` to [`Self::process`], which starts
/// the same fade from the last published sample. Output remains silent until
/// the caller has fresh startup data and calls [`Self::begin_recovery`].
#[derive(Clone, Copy, Debug)]
pub struct FaultRamp {
    state: FaultRampState,
    gain_step: u16,
    last_output: f32,
}

impl Default for FaultRamp {
    fn default() -> Self {
        Self::new()
    }
}

impl FaultRamp {
    /// Creates a ramp passing valid processed audio at unity gain.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: FaultRampState::Passing,
            gain_step: FAULT_RAMP_SAMPLES,
            last_output: 0.0,
        }
    }

    /// Returns the current transition state.
    #[must_use]
    pub const fn state(&self) -> FaultRampState {
        self.state
    }

    /// Starts or reverses a transition toward fail-closed silence.
    pub const fn begin_fault(&mut self) {
        if !matches!(self.state, FaultRampState::Silent) {
            self.state = FaultRampState::FadingOut;
        }
    }

    /// Starts or reverses a transition toward fresh processed audio.
    pub const fn begin_recovery(&mut self) {
        if !matches!(self.state, FaultRampState::Passing) {
            self.state = FaultRampState::FadingIn;
        }
    }

    /// Clears history and returns immediately to fail-closed silence.
    pub const fn reset_silent(&mut self) {
        self.state = FaultRampState::Silent;
        self.gain_step = 0;
        self.last_output = 0.0;
    }

    /// Renders one bounded callback from processed input or an underflow.
    ///
    /// Missing input automatically begins a fade to silence and never replays a
    /// prior buffer; only the last published scalar is used as the ramp origin.
    /// Present input remains muted in [`FaultRampState::Silent`] until explicit
    /// recovery. Shape errors clear output and move directly to silence.
    ///
    /// # Errors
    ///
    /// Returns an error for mismatched present-input/output shapes or a callback
    /// longer than [`MAX_CALLBACK_FRAMES`]. Output is silence on error.
    pub fn process(
        &mut self,
        input: Option<&[f32]>,
        output: &mut [f32],
    ) -> Result<FaultRampReport, FaultRampError> {
        if output.len() > MAX_CALLBACK_FRAMES {
            output.fill(0.0);
            self.reset_silent();
            return Err(FaultRampError::QuantumTooLarge);
        }
        if input.is_some_and(|samples| samples.len() != output.len()) {
            output.fill(0.0);
            self.reset_silent();
            return Err(FaultRampError::ShapeMismatch);
        }
        if input.is_none() {
            self.begin_fault();
        }

        let held_sample = self.last_output;
        let mut report = FaultRampReport {
            missing_samples: if input.is_none() {
                u16::try_from(output.len()).unwrap_or(u16::MAX)
            } else {
                0
            },
            ..FaultRampReport::default()
        };
        if let Some(input) = input {
            for (source, destination) in input.iter().zip(output.iter_mut()) {
                self.render_sample(*source, destination, &mut report.sanitized);
            }
        } else {
            for destination in output.iter_mut() {
                self.render_sample(held_sample, destination, &mut report.sanitized);
            }
        }
        report.state = self.state;
        Ok(report)
    }

    fn render_sample(
        &mut self,
        source: f32,
        destination: &mut f32,
        sanitized: &mut SanitizeReport,
    ) {
        let source = sanitize_sample(source, sanitized);
        let gain = self.next_gain();
        *destination = sanitize_sample(source * gain, sanitized);
        self.last_output = *destination;
    }

    fn next_gain(&mut self) -> f32 {
        match self.state {
            FaultRampState::Passing => 1.0,
            FaultRampState::Silent => 0.0,
            FaultRampState::FadingOut => {
                self.gain_step = self.gain_step.saturating_sub(1);
                if self.gain_step == 0 {
                    self.state = FaultRampState::Silent;
                }
                self.current_gain()
            }
            FaultRampState::FadingIn => {
                self.gain_step = self.gain_step.saturating_add(1).min(FAULT_RAMP_SAMPLES);
                if self.gain_step == FAULT_RAMP_SAMPLES {
                    self.state = FaultRampState::Passing;
                }
                self.current_gain()
            }
        }
    }

    fn current_gain(self) -> f32 {
        (HALF_PI * f32::from(self.gain_step) / f32::from(FAULT_RAMP_SAMPLES)).sin()
    }
}

/// A click-detector shape error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClickDetectorError {
    /// Reference and observed buffers have different lengths.
    ShapeMismatch,
}

impl fmt::Display for ClickDetectorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("click-detector reference and observed lengths differ")
    }
}

impl std::error::Error for ClickDetectorError {}

/// Detects transition-induced steps beyond source-signal continuity.
///
/// The detector compares adjacent-step magnitude, not sample values. A click is
/// counted when the observed step exceeds the reference step by more than
/// [`CLICK_EXCESS_THRESHOLD`] (0.01 full scale, approximately -40 dBFS).
#[derive(Clone, Copy, Debug, Default)]
pub struct ClickDetector {
    primed: bool,
    previous_reference: f32,
    previous_observed: f32,
    maximum_excess: f32,
    clicks: u64,
}

impl ClickDetector {
    /// Creates an empty detector.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            primed: false,
            previous_reference: 0.0,
            previous_observed: 0.0,
            maximum_excess: 0.0,
            clicks: 0,
        }
    }

    /// Observes aligned reference and transition output buffers.
    ///
    /// # Errors
    ///
    /// Returns [`ClickDetectorError::ShapeMismatch`] for unequal lengths.
    pub fn observe(
        &mut self,
        reference: &[f32],
        observed: &[f32],
    ) -> Result<(), ClickDetectorError> {
        if reference.len() != observed.len() {
            return Err(ClickDetectorError::ShapeMismatch);
        }
        for (reference, observed) in reference.iter().zip(observed.iter()) {
            if !reference.is_finite() || !observed.is_finite() {
                self.clicks += 1;
                self.maximum_excess = f32::INFINITY;
            } else if self.primed {
                let reference_step = (*reference - self.previous_reference).abs();
                let observed_step = (*observed - self.previous_observed).abs();
                let excess = (observed_step - reference_step).max(0.0);
                self.maximum_excess = self.maximum_excess.max(excess);
                if excess > CLICK_EXCESS_THRESHOLD {
                    self.clicks += 1;
                }
            }
            self.previous_reference = *reference;
            self.previous_observed = *observed;
            self.primed = true;
        }
        Ok(())
    }

    /// Returns the largest detected excess adjacent-sample step.
    #[must_use]
    pub const fn maximum_excess(&self) -> f32 {
        self.maximum_excess
    }

    /// Returns the count of threshold violations.
    #[must_use]
    pub const fn clicks(&self) -> u64 {
        self.clicks
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::{ClickDetector, FaultRamp, FaultRampError, FaultRampState};
    use crate::{CLICK_EXCESS_THRESHOLD, FAULT_RAMP_SAMPLES};

    #[test]
    fn underflow_fades_to_silence_without_click_or_replay() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut ramp = FaultRamp::new();
        let reference = [0.8; FAULT_RAMP_SAMPLES as usize];
        let mut priming = [0.0; 1];
        ramp.process(Some(&[0.8]), &mut priming)?;
        let mut output = [0.0; FAULT_RAMP_SAMPLES as usize];
        let report = ramp.process(None, &mut output)?;
        let mut detector = ClickDetector::new();
        detector.observe(&reference, &output)?;

        assert_eq!(report.missing_samples, FAULT_RAMP_SAMPLES);
        assert_eq!(report.state, FaultRampState::Silent);
        assert_eq!(output[FAULT_RAMP_SAMPLES as usize - 1], 0.0);
        assert_eq!(detector.clicks(), 0);
        assert!(detector.maximum_excess() <= CLICK_EXCESS_THRESHOLD);

        let mut still_silent = [1.0; 32];
        ramp.process(Some(&[0.8; 32]), &mut still_silent)?;
        assert_eq!(still_silent, [0.0; 32]);
        Ok(())
    }

    #[test]
    fn overflow_drain_and_fresh_recovery_are_click_bounded()
    -> Result<(), Box<dyn std::error::Error>> {
        let reference = [0.75; FAULT_RAMP_SAMPLES as usize];
        let mut ramp = FaultRamp::new();
        let mut fading = [0.0; FAULT_RAMP_SAMPLES as usize];
        ramp.begin_fault();
        ramp.process(Some(&reference), &mut fading)?;
        assert_eq!(ramp.state(), FaultRampState::Silent);

        ramp.begin_recovery();
        let mut recovery = [0.0; FAULT_RAMP_SAMPLES as usize];
        ramp.process(Some(&reference), &mut recovery)?;
        assert_eq!(ramp.state(), FaultRampState::Passing);

        let mut fade_detector = ClickDetector::new();
        fade_detector.observe(&reference, &fading)?;
        let mut recovery_detector = ClickDetector::new();
        recovery_detector.observe(&reference, &recovery)?;
        assert_eq!(fade_detector.clicks(), 0);
        assert_eq!(recovery_detector.clicks(), 0);
        Ok(())
    }

    #[test]
    fn detector_rejects_an_abrupt_full_scale_cut() -> Result<(), Box<dyn std::error::Error>> {
        let reference = [0.8; 4];
        let observed = [0.8, 0.8, 0.0, 0.0];
        let mut detector = ClickDetector::new();
        detector.observe(&reference, &observed)?;
        assert_eq!(detector.clicks(), 1);
        assert!(detector.maximum_excess() > CLICK_EXCESS_THRESHOLD);
        Ok(())
    }

    #[test]
    fn malformed_shapes_fail_closed() {
        let mut ramp = FaultRamp::new();
        let mut output = [1.0; 4];
        assert_eq!(
            ramp.process(Some(&[0.0; 3]), &mut output),
            Err(FaultRampError::ShapeMismatch)
        );
        assert_eq!(output, [0.0; 4]);
        assert_eq!(ramp.state(), FaultRampState::Silent);
    }
}
