//! Stateful one-pole DC blocking.

use crate::{SAMPLE_RATE_HZ, SanitizeReport, sanitize_sample};

const DEFAULT_CUTOFF_HZ: f32 = 60.0;

/// An allocation-free one-pole high-pass filter near 60 Hz.
#[derive(Clone, Debug)]
pub struct DcBlocker {
    coefficient: f32,
    previous_input: f32,
    previous_output: f32,
    enabled: bool,
}

impl Default for DcBlocker {
    fn default() -> Self {
        Self::new()
    }
}

impl DcBlocker {
    /// Creates an enabled blocker configured for the canonical sample rate.
    #[must_use]
    pub fn new() -> Self {
        let sample_rate = 48_000.0;
        debug_assert_eq!(SAMPLE_RATE_HZ, 48_000);
        let radians = -2.0 * core::f32::consts::PI * DEFAULT_CUTOFF_HZ / sample_rate;
        Self {
            coefficient: radians.exp(),
            previous_input: 0.0,
            previous_output: 0.0,
            enabled: true,
        }
    }

    /// Enables or disables filtering. Disabled processing still sanitizes input.
    pub const fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Returns whether filtering is active.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Clears filter history without changing the enabled state.
    pub const fn reset(&mut self) {
        self.previous_input = 0.0;
        self.previous_output = 0.0;
    }

    /// Processes samples in place and returns sanitization counters.
    pub fn process(&mut self, samples: &mut [f32]) -> SanitizeReport {
        let mut report = SanitizeReport::default();
        for sample in samples {
            let input = sanitize_sample(*sample, &mut report);
            if self.enabled {
                let output = input - self.previous_input + self.coefficient * self.previous_output;
                self.previous_input = input;
                self.previous_output = sanitize_sample(output, &mut report);
                *sample = self.previous_output;
            } else {
                *sample = input;
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::{DEFAULT_CUTOFF_HZ, DcBlocker};

    #[test]
    fn production_cutoff_is_quality_audited_sixty_hz() {
        assert_eq!(DEFAULT_CUTOFF_HZ, 60.0);
    }

    #[test]
    fn constant_offset_decays_toward_zero() {
        let mut blocker = DcBlocker::new();
        let mut samples = vec![1.0; 4_800];
        blocker.process(&mut samples);
        assert!(samples[4_799].abs() < 0.001);
    }

    #[test]
    fn disabled_mode_is_sanitized_parity() {
        let mut blocker = DcBlocker::new();
        blocker.set_enabled(false);
        let mut samples = [0.25, -0.5, f32::NAN];
        let report = blocker.process(&mut samples);
        assert_eq!(samples, [0.25, -0.5, 0.0]);
        assert_eq!(report.non_finite, 1);
    }

    #[test]
    fn reset_restarts_filter_history() {
        let mut blocker = DcBlocker::new();
        let mut first = [0.75];
        blocker.process(&mut first);
        blocker.reset();
        let mut second = [0.75];
        blocker.process(&mut second);
        assert_eq!(first, second);
    }
}
