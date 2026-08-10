//! Allocation-free audio framing and signal processing for Noire.
//!
//! The types in this crate define Noire's canonical mono, 48 kHz audio domain.
//! Stateful processors allocate no memory after construction and accept bounded
//! callback chunks.

#![forbid(unsafe_code)]

mod canonical;
mod channels;
mod dc;
mod delay;
mod frame;
mod meter;
mod ramp;
mod sanitize;

pub use canonical::{
    CANONICAL_CHANNELS, CANONICAL_FORMAT, CanonicalFormat, CanonicalSample,
    FRAME_ASSEMBLER_CAPACITY, MAX_CALLBACK_FRAMES, MAX_DRY_DELAY_SAMPLES,
    MIN_STRENGTH_RAMP_SAMPLES, MODEL_FRAME_SAMPLES, ModelFrame, SAMPLE_RATE_HZ,
};
pub use channels::{
    ChannelMap, ChannelMapError, ChannelPosition, ChannelSelection, DownmixReport,
    MAX_INPUT_CHANNELS,
};
pub use dc::DcBlocker;
pub use delay::{DryDelay, DryDelayError};
pub use frame::{FrameAssembler, FrameAssemblerError, FramePushReport};
pub use meter::{Meter, MeterSnapshot};
pub use ramp::{EqualPowerMixer, MixReport, StrengthRamp};
pub use sanitize::{SanitizeReport, sanitize_buffer, sanitize_sample};
