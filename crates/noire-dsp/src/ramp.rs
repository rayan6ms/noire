//! Bounded click-safe control ramps and wet/dry mixing.

use crate::{MIN_STRENGTH_RAMP_SAMPLES, SanitizeReport, sanitize_sample};

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

/// Counts sanitization and transparent hard-ceiling events during mixing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MixReport {
    /// Invalid or denormal samples replaced with zero.
    pub sanitized: SanitizeReport,
    /// Finite mixed samples constrained to the canonical range.
    pub hard_ceiling: u64,
}

/// A stateless equal-power wet/dry mixer.
#[derive(Clone, Copy, Debug, Default)]
pub struct EqualPowerMixer;

impl EqualPowerMixer {
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
            dry * (1.0 - strength).sqrt() + wet * strength.sqrt()
        };
        let mixed = sanitize_sample(mixed, &mut report.sanitized);
        let limited = mixed.clamp(-1.0, 1.0);
        if !(-1.0..=1.0).contains(&mixed) {
            report.hard_ceiling += 1;
        }
        limited
    }
}

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

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::{EqualPowerMixer, MixReport, StrengthRamp};
    use crate::MIN_STRENGTH_RAMP_SAMPLES;

    #[test]
    fn strength_endpoints_are_exact() {
        let mut report = MixReport::default();
        assert_eq!(EqualPowerMixer::mix(0.25, -0.75, 0.0, &mut report), 0.25);
        assert_eq!(EqualPowerMixer::mix(0.25, -0.75, 1.0, &mut report), -0.75);
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
        assert_eq!(EqualPowerMixer::mix(f32::NAN, 0.5, 0.0, &mut report), 0.0);
        assert_eq!(EqualPowerMixer::mix(1.0, 1.0, 0.5, &mut report), 1.0);
        assert_eq!(report.sanitized.non_finite, 1);
        assert_eq!(report.hard_ceiling, 1);
    }
}
