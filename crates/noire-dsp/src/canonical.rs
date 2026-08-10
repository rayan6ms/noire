//! Canonical audio-domain constants and types.

/// The number of samples processed each second.
pub const SAMPLE_RATE_HZ: u32 = 48_000;

/// The number of channels in Noire's internal audio domain.
pub const CANONICAL_CHANNELS: usize = 1;

/// The exact 10 ms model frame size at 48 kHz.
pub const MODEL_FRAME_SAMPLES: usize = 480;

/// The largest callback quantum accepted by bounded DSP utilities.
pub const MAX_CALLBACK_FRAMES: usize = 4_096;

/// The initial bounded storage budget for the frame assembler.
pub const FRAME_ASSEMBLER_CAPACITY: usize = MODEL_FRAME_SAMPLES * 2;

/// The shortest allowed strength transition, equal to 20 ms at 48 kHz.
pub const MIN_STRENGTH_RAMP_SAMPLES: u32 = 960;

/// The 5 ms fade used for overflow, underflow, and recovery transitions.
pub const FAULT_RAMP_SAMPLES: u16 = 240;

/// Maximum transition-induced adjacent-sample step above source continuity.
pub const CLICK_EXCESS_THRESHOLD: f32 = 0.01;

/// The largest supported dry delay: one model frame plus one callback quantum.
pub const MAX_DRY_DELAY_SAMPLES: usize = MODEL_FRAME_SAMPLES + MAX_CALLBACK_FRAMES;

/// A sample in the canonical normalized floating-point domain.
pub type CanonicalSample = f32;

/// One exact model frame in the canonical domain.
pub type ModelFrame = [CanonicalSample; MODEL_FRAME_SAMPLES];

/// The immutable description of Noire's internal audio format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanonicalFormat {
    /// Samples per second.
    pub sample_rate_hz: u32,
    /// Interleaved channel count; always one internally.
    pub channels: usize,
    /// Samples in one model frame.
    pub model_frame_samples: usize,
}

/// Noire's canonical mono, 48 kHz, 480-sample-frame format.
pub const CANONICAL_FORMAT: CanonicalFormat = CanonicalFormat {
    sample_rate_hz: SAMPLE_RATE_HZ,
    channels: CANONICAL_CHANNELS,
    model_frame_samples: MODEL_FRAME_SAMPLES,
};

#[cfg(test)]
mod tests {
    use super::{CANONICAL_FORMAT, MODEL_FRAME_SAMPLES, SAMPLE_RATE_HZ};

    #[test]
    fn canonical_format_is_mono_48_khz_with_ten_ms_frames() {
        assert_eq!(CANONICAL_FORMAT.sample_rate_hz, SAMPLE_RATE_HZ);
        assert_eq!(CANONICAL_FORMAT.channels, 1);
        assert_eq!(CANONICAL_FORMAT.model_frame_samples, MODEL_FRAME_SAMPLES);
        assert_eq!(MODEL_FRAME_SAMPLES, 480);
    }
}
