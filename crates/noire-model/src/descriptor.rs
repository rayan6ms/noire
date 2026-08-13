//! Immutable model identity, format, and timing metadata.

use core::fmt;

/// Unvalidated metadata used to construct a [`ModelDescriptor`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelDescriptorSpec {
    /// Stable machine-readable model/adapter identifier.
    pub id: &'static str,
    /// Human-readable model name.
    pub name: &'static str,
    /// Stable model or weights version.
    pub version: &'static str,
    /// SPDX license expression for the implementation and distributed weights.
    pub license: &'static str,
    /// Required sample rate.
    pub sample_rate_hz: u32,
    /// Required channel count.
    pub channels: u16,
    /// Samples per channel in one complete input/output frame.
    pub frame_samples: usize,
    /// Samples per channel advanced by one processing call.
    pub hop_samples: usize,
    /// Future samples per channel required by the algorithm.
    pub lookahead_samples: usize,
    /// Total algorithmic output delay in samples per channel.
    pub delay_samples: usize,
}

/// A descriptor validation error with no heap-backed payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DescriptorError {
    /// The stable identifier is empty or whitespace.
    EmptyId,
    /// The human-readable name is empty or whitespace.
    EmptyName,
    /// The version is empty or whitespace.
    EmptyVersion,
    /// The SPDX license expression is empty or whitespace.
    EmptyLicense,
    /// Sample rate, channels, or frame length is zero.
    InvalidFormat,
    /// Hop length is zero or exceeds frame length.
    InvalidHop,
    /// Total delay is shorter than declared lookahead.
    InvalidDelay,
    /// Interleaved frame sample count cannot be represented.
    FrameSizeOverflow,
}

impl fmt::Display for DescriptorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyId => "model identifier is empty",
            Self::EmptyName => "model name is empty",
            Self::EmptyVersion => "model version is empty",
            Self::EmptyLicense => "model license expression is empty",
            Self::InvalidFormat => "model format contains a zero value",
            Self::InvalidHop => "model hop length is outside the frame",
            Self::InvalidDelay => "model delay is shorter than its lookahead",
            Self::FrameSizeOverflow => "model interleaved frame size overflows",
        })
    }
}

impl std::error::Error for DescriptorError {}

/// Validated immutable model identity, format, timing, and license metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelDescriptor {
    spec: ModelDescriptorSpec,
    frame_buffer_samples: usize,
}

impl ModelDescriptor {
    /// Validates and constructs immutable model metadata.
    ///
    /// # Errors
    ///
    /// Returns [`DescriptorError`] for empty identity/license fields, invalid
    /// format/timing values, or an interleaved frame-size overflow.
    pub fn new(spec: ModelDescriptorSpec) -> Result<Self, DescriptorError> {
        if spec.id.trim().is_empty() {
            return Err(DescriptorError::EmptyId);
        }
        if spec.name.trim().is_empty() {
            return Err(DescriptorError::EmptyName);
        }
        if spec.version.trim().is_empty() {
            return Err(DescriptorError::EmptyVersion);
        }
        if spec.license.trim().is_empty() {
            return Err(DescriptorError::EmptyLicense);
        }
        if spec.sample_rate_hz == 0 || spec.channels == 0 || spec.frame_samples == 0 {
            return Err(DescriptorError::InvalidFormat);
        }
        if spec.hop_samples == 0 || spec.hop_samples > spec.frame_samples {
            return Err(DescriptorError::InvalidHop);
        }
        if spec.delay_samples < spec.lookahead_samples {
            return Err(DescriptorError::InvalidDelay);
        }
        let frame_buffer_samples = spec
            .frame_samples
            .checked_mul(usize::from(spec.channels))
            .ok_or(DescriptorError::FrameSizeOverflow)?;

        Ok(Self {
            spec,
            frame_buffer_samples,
        })
    }

    /// Returns the stable machine-readable identifier.
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.spec.id
    }

    /// Returns the human-readable model name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.spec.name
    }

    /// Returns the model or weights version.
    #[must_use]
    pub const fn version(&self) -> &'static str {
        self.spec.version
    }

    /// Returns the SPDX license expression.
    #[must_use]
    pub const fn license(&self) -> &'static str {
        self.spec.license
    }

    /// Returns the required sample rate.
    #[must_use]
    pub const fn sample_rate_hz(&self) -> u32 {
        self.spec.sample_rate_hz
    }

    /// Returns the required channel count.
    #[must_use]
    pub const fn channels(&self) -> u16 {
        self.spec.channels
    }

    /// Returns samples per channel in a complete frame.
    #[must_use]
    pub const fn frame_samples(&self) -> usize {
        self.spec.frame_samples
    }

    /// Returns samples per channel advanced by one processing call.
    #[must_use]
    pub const fn hop_samples(&self) -> usize {
        self.spec.hop_samples
    }

    /// Returns required future samples per channel.
    #[must_use]
    pub const fn lookahead_samples(&self) -> usize {
        self.spec.lookahead_samples
    }

    /// Returns total algorithmic output delay per channel.
    #[must_use]
    pub const fn delay_samples(&self) -> usize {
        self.spec.delay_samples
    }

    /// Returns the exact interleaved input/output slice length.
    #[must_use]
    pub const fn frame_buffer_samples(&self) -> usize {
        self.frame_buffer_samples
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{DescriptorError, ModelDescriptor, ModelDescriptorSpec};

    proptest! {
        #[test]
        fn arbitrary_manifest_numbers_are_rejected_or_bounded(
            sample_rate_hz in any::<u32>(),
            channels in any::<u16>(),
            frame_samples in any::<usize>(),
            hop_samples in any::<usize>(),
            lookahead_samples in any::<usize>(),
            delay_samples in any::<usize>(),
        ) {
            let result = ModelDescriptor::new(ModelDescriptorSpec {
                id: "org.noire.fuzz",
                name: "Fuzz model",
                version: "1",
                license: "MIT",
                sample_rate_hz,
                channels,
                frame_samples,
                hop_samples,
                lookahead_samples,
                delay_samples,
            });
            if let Ok(descriptor) = result {
                prop_assert!(descriptor.sample_rate_hz() > 0);
                prop_assert!(descriptor.channels() > 0);
                prop_assert!(descriptor.frame_samples() > 0);
                prop_assert!(descriptor.hop_samples() <= descriptor.frame_samples());
                prop_assert!(descriptor.delay_samples() >= descriptor.lookahead_samples());
                prop_assert_eq!(
                    descriptor.frame_buffer_samples(),
                    descriptor.frame_samples() * usize::from(descriptor.channels())
                );
            }
        }
    }

    fn valid_spec() -> ModelDescriptorSpec {
        ModelDescriptorSpec {
            id: "org.example.model",
            name: "Example",
            version: "1.0",
            license: "BSD-3-Clause",
            sample_rate_hz: 48_000,
            channels: 1,
            frame_samples: 480,
            hop_samples: 480,
            lookahead_samples: 0,
            delay_samples: 480,
        }
    }

    #[test]
    fn rejects_empty_identity_metadata() {
        let mut spec = valid_spec();
        spec.id = "  ";
        assert_eq!(ModelDescriptor::new(spec), Err(DescriptorError::EmptyId));

        spec = valid_spec();
        spec.license = "";
        assert_eq!(
            ModelDescriptor::new(spec),
            Err(DescriptorError::EmptyLicense)
        );
    }

    #[test]
    fn rejects_invalid_format_hop_and_delay() {
        let mut spec = valid_spec();
        spec.channels = 0;
        assert_eq!(
            ModelDescriptor::new(spec),
            Err(DescriptorError::InvalidFormat)
        );

        spec = valid_spec();
        spec.hop_samples = 481;
        assert_eq!(ModelDescriptor::new(spec), Err(DescriptorError::InvalidHop));

        spec = valid_spec();
        spec.lookahead_samples = 481;
        assert_eq!(
            ModelDescriptor::new(spec),
            Err(DescriptorError::InvalidDelay)
        );
    }

    #[test]
    fn computes_interleaved_frame_length_once() -> Result<(), DescriptorError> {
        let mut spec = valid_spec();
        spec.channels = 2;
        let descriptor = ModelDescriptor::new(spec)?;
        assert_eq!(descriptor.frame_buffer_samples(), 960);
        Ok(())
    }
}
