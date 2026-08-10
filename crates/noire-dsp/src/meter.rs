//! Constant-work peak and RMS metering.

use crate::{SanitizeReport, sanitize_sample};

/// The largest accumulation window before a snapshot must be taken.
const MAX_METER_WINDOW_SAMPLES: u16 = 48_000;

/// An immutable peak/RMS meter reading.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeterSnapshot {
    /// Absolute sample peak.
    pub peak: f32,
    /// Root-mean-square amplitude.
    pub rms: f32,
    /// Samples represented by this reading.
    pub samples: u16,
    /// Invalid or denormal samples treated as silence.
    pub sanitized: SanitizeReport,
}

/// A bounded, allocation-free peak and online-RMS accumulator.
#[derive(Clone, Copy, Debug, Default)]
pub struct Meter {
    peak: f32,
    mean_square: f32,
    samples: u16,
    sanitized: SanitizeReport,
}

impl Meter {
    /// Creates an empty meter window.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            peak: 0.0,
            mean_square: 0.0,
            samples: 0,
            sanitized: SanitizeReport {
                non_finite: 0,
                subnormal: 0,
            },
        }
    }

    /// Adds samples with fixed work per input sample.
    ///
    /// Once the one-second safety bound is reached, further samples are ignored
    /// until [`Self::take_snapshot`] resets the window.
    pub fn observe(&mut self, input: &[f32]) {
        for sample in input {
            if self.samples == MAX_METER_WINDOW_SAMPLES {
                break;
            }
            let sample = sanitize_sample(*sample, &mut self.sanitized);
            self.peak = self.peak.max(sample.abs());
            self.samples += 1;
            let count = f32::from(self.samples);
            self.mean_square += (sample.mul_add(sample, -self.mean_square)) / count;
        }
    }

    /// Returns the current reading without resetting it.
    #[must_use]
    pub fn snapshot(&self) -> MeterSnapshot {
        MeterSnapshot {
            peak: self.peak,
            rms: self.mean_square.max(0.0).sqrt(),
            samples: self.samples,
            sanitized: self.sanitized,
        }
    }

    /// Returns the current reading and starts a fresh window.
    pub fn take_snapshot(&mut self) -> MeterSnapshot {
        let snapshot = self.snapshot();
        *self = Self::new();
        snapshot
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::Meter;

    #[test]
    fn computes_known_peak_and_rms() {
        let mut meter = Meter::new();
        meter.observe(&[1.0, -1.0, 0.0, 0.0]);
        let snapshot = meter.snapshot();
        assert_eq!(snapshot.peak, 1.0);
        assert!((snapshot.rms - core::f32::consts::FRAC_1_SQRT_2).abs() < 1.0e-6);
        assert_eq!(snapshot.samples, 4);
    }

    #[test]
    fn snapshot_reset_is_explicit() {
        let mut meter = Meter::new();
        meter.observe(&[0.5, f32::NAN]);
        let first = meter.take_snapshot();
        let second = meter.snapshot();
        assert_eq!(first.samples, 2);
        assert_eq!(first.sanitized.non_finite, 1);
        assert_eq!(second.samples, 0);
        assert_eq!(second.peak, 0.0);
    }
}
