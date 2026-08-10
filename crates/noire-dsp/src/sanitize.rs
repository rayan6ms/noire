//! Finite-value and denormal sanitization.

/// Counts samples changed while crossing a DSP trust boundary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SanitizeReport {
    /// NaN or infinity samples replaced with zero.
    pub non_finite: u64,
    /// Subnormal samples flushed to zero.
    pub subnormal: u64,
}

impl SanitizeReport {
    /// Returns the total number of replaced samples.
    #[must_use]
    pub const fn replaced(self) -> u64 {
        self.non_finite + self.subnormal
    }
}

/// Converts a sample to a finite, non-subnormal value and updates `report`.
#[must_use]
pub fn sanitize_sample(sample: f32, report: &mut SanitizeReport) -> f32 {
    if !sample.is_finite() {
        report.non_finite += 1;
        0.0
    } else if sample != 0.0 && sample.is_subnormal() {
        report.subnormal += 1;
        0.0
    } else {
        sample
    }
}

/// Sanitizes a buffer in place and returns replacement counters.
pub fn sanitize_buffer(samples: &mut [f32]) -> SanitizeReport {
    let mut report = SanitizeReport::default();
    for sample in samples {
        *sample = sanitize_sample(*sample, &mut report);
    }
    report
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::{SanitizeReport, sanitize_buffer, sanitize_sample};
    use proptest::prelude::*;

    #[test]
    fn replaces_non_finite_and_subnormal_values() {
        let mut samples = [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::from_bits(1),
            -0.0,
            0.25,
        ];
        let report = sanitize_buffer(&mut samples);

        assert_eq!(report.non_finite, 3);
        assert_eq!(report.subnormal, 1);
        assert_eq!(samples, [0.0, 0.0, 0.0, 0.0, -0.0, 0.25]);
    }

    proptest! {
        #[test]
        fn every_bit_pattern_becomes_finite_and_normal_or_zero(bits in any::<u32>()) {
            let mut report = SanitizeReport::default();
            let output = sanitize_sample(f32::from_bits(bits), &mut report);
            prop_assert!(output.is_finite());
            prop_assert!(output == 0.0 || output.is_normal());
        }
    }
}
