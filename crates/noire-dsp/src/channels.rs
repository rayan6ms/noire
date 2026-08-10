//! Explicit interleaved-channel mapping into canonical mono audio.

use core::fmt;

use crate::{MAX_CALLBACK_FRAMES, SanitizeReport, sanitize_sample};

/// The largest channel count accepted at the audio boundary.
pub const MAX_INPUT_CHANNELS: usize = 64;

/// A semantic channel position supplied by the audio graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelPosition {
    /// A declared mono channel.
    Mono,
    /// A front-center channel.
    FrontCenter,
    /// A front-left channel.
    FrontLeft,
    /// A front-right channel.
    FrontRight,
    /// Any other known or unknown position.
    Other,
}

/// The policy used to choose channels for canonical mono audio.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ChannelSelection {
    /// Prefer the first mono/front-center channel, otherwise mix every channel.
    #[default]
    Auto,
    /// Select exactly one zero-based input channel.
    Channel(usize),
    /// Mix every input channel, even when mono/front-center is present.
    MixAll,
}

/// A channel-map construction or processing error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelMapError {
    /// No input channels were described.
    NoChannels,
    /// The input has more channels than the fixed boundary permits.
    TooManyChannels,
    /// A manually selected index does not exist.
    InvalidSelection,
    /// Input/output lengths do not describe the configured number of frames.
    ShapeMismatch,
    /// The callback quantum exceeds the fixed processing bound.
    QuantumTooLarge,
}

impl fmt::Display for ChannelMapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NoChannels => "channel map requires at least one channel",
            Self::TooManyChannels => "channel count exceeds the fixed boundary",
            Self::InvalidSelection => "selected channel does not exist",
            Self::ShapeMismatch => "interleaved input and mono output shapes do not match",
            Self::QuantumTooLarge => "callback quantum exceeds the fixed boundary",
        })
    }
}

impl std::error::Error for ChannelMapError {}

/// Counters from one downmix operation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DownmixReport {
    /// Frames written to canonical mono output.
    pub frames: usize,
    /// Invalid or denormal input samples replaced with zero.
    pub sanitized: SanitizeReport,
}

/// A precomputed, allocation-free channel map.
#[derive(Clone, Debug)]
pub struct ChannelMap {
    channel_count: usize,
    selected: [bool; MAX_INPUT_CHANNELS],
    selected_count: usize,
    gain: f32,
}

impl ChannelMap {
    /// Builds a fixed map from semantic positions and a selection policy.
    ///
    /// Mixed channels first receive equal-power normalization and then an equal
    /// coherent-signal headroom factor. The resulting `1 / channel_count` gain
    /// prevents identical full-scale inputs from clipping.
    ///
    /// # Errors
    ///
    /// Returns an error when there are no channels, the fixed channel bound is
    /// exceeded, or a manual selection is outside the described map.
    pub fn new(
        positions: &[ChannelPosition],
        selection: ChannelSelection,
    ) -> Result<Self, ChannelMapError> {
        if positions.is_empty() {
            return Err(ChannelMapError::NoChannels);
        }
        if positions.len() > MAX_INPUT_CHANNELS {
            return Err(ChannelMapError::TooManyChannels);
        }

        let mut selected = [false; MAX_INPUT_CHANNELS];
        match selection {
            ChannelSelection::Auto => {
                if let Some(index) = positions.iter().position(|position| {
                    matches!(
                        position,
                        ChannelPosition::Mono | ChannelPosition::FrontCenter
                    )
                }) {
                    selected[index] = true;
                } else {
                    selected[..positions.len()].fill(true);
                }
            }
            ChannelSelection::Channel(index) => {
                if index >= positions.len() {
                    return Err(ChannelMapError::InvalidSelection);
                }
                selected[index] = true;
            }
            ChannelSelection::MixAll => selected[..positions.len()].fill(true),
        }

        let selected_count_u8 = selected[..positions.len()]
            .iter()
            .fold(0_u8, |count, is_selected| count + u8::from(*is_selected));
        let selected_count = usize::from(selected_count_u8);
        let gain = 1.0 / f32::from(selected_count_u8);

        Ok(Self {
            channel_count: positions.len(),
            selected,
            selected_count,
            gain,
        })
    }

    /// Returns the number of interleaved input channels.
    #[must_use]
    pub const fn channel_count(&self) -> usize {
        self.channel_count
    }

    /// Returns the number of channels contributing to mono output.
    #[must_use]
    pub const fn selected_count(&self) -> usize {
        self.selected_count
    }

    /// Maps interleaved input into a same-frame-count mono output buffer.
    ///
    /// # Errors
    ///
    /// Returns an error when the shapes do not match the configured map or the
    /// callback quantum exceeds [`MAX_CALLBACK_FRAMES`].
    pub fn process(
        &self,
        interleaved: &[f32],
        mono: &mut [f32],
    ) -> Result<DownmixReport, ChannelMapError> {
        if mono.len() > MAX_CALLBACK_FRAMES {
            return Err(ChannelMapError::QuantumTooLarge);
        }
        if mono
            .len()
            .checked_mul(self.channel_count)
            .is_none_or(|expected| expected != interleaved.len())
        {
            return Err(ChannelMapError::ShapeMismatch);
        }

        let mut sanitized = SanitizeReport::default();
        for (frame, output) in interleaved
            .chunks_exact(self.channel_count)
            .zip(mono.iter_mut())
        {
            let mut mixed = 0.0;
            for (index, sample) in frame.iter().enumerate() {
                if self.selected[index] {
                    mixed += sanitize_sample(*sample, &mut sanitized) * self.gain;
                }
            }
            *output = sanitize_sample(mixed, &mut sanitized);
        }

        Ok(DownmixReport {
            frames: mono.len(),
            sanitized,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::{ChannelMap, ChannelMapError, ChannelPosition, ChannelSelection};

    #[test]
    fn auto_prefers_first_declared_mono_or_front_center() -> Result<(), ChannelMapError> {
        let map = ChannelMap::new(
            &[
                ChannelPosition::FrontLeft,
                ChannelPosition::FrontCenter,
                ChannelPosition::Mono,
            ],
            ChannelSelection::Auto,
        )?;
        let mut output = [0.0; 2];
        map.process(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6], &mut output)?;
        assert_eq!(output, [0.2, 0.5]);
        Ok(())
    }

    #[test]
    fn fallback_mix_preserves_coherent_headroom() -> Result<(), ChannelMapError> {
        let map = ChannelMap::new(
            &[ChannelPosition::FrontLeft, ChannelPosition::FrontRight],
            ChannelSelection::Auto,
        )?;
        let mut output = [0.0; 2];
        map.process(&[1.0, 1.0, -1.0, -1.0], &mut output)?;
        assert_eq!(output, [1.0, -1.0]);
        Ok(())
    }

    #[test]
    fn manual_channel_selection_is_exact() -> Result<(), ChannelMapError> {
        let map = ChannelMap::new(
            &[ChannelPosition::FrontLeft, ChannelPosition::FrontRight],
            ChannelSelection::Channel(1),
        )?;
        let mut output = [0.0; 2];
        map.process(&[0.2, 0.7, 0.3, -0.6], &mut output)?;
        assert_eq!(output, [0.7, -0.6]);
        Ok(())
    }

    #[test]
    fn rejects_malformed_buffers() -> Result<(), ChannelMapError> {
        let map = ChannelMap::new(&[ChannelPosition::Mono], ChannelSelection::Auto)?;
        let error = map.process(&[0.0, 1.0], &mut [0.0]);
        assert_eq!(error, Err(ChannelMapError::ShapeMismatch));
        Ok(())
    }
}
