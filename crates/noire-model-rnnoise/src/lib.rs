//! `RNNoise` model adapter for Noire.
//!
//! Enable the `rnnoise` feature to expose the embedded-default-model factory.

#![forbid(unsafe_code)]

#[cfg(feature = "rnnoise")]
mod adapter;

#[cfg(feature = "offline-wav")]
mod offline;

#[cfg(feature = "rnnoise")]
pub use adapter::{
    DEFAULT_WEIGHTS_SHA256, RNNOISE_DELAY_SAMPLES, RNNOISE_FRAME_SAMPLES, RNNOISE_SAMPLE_RATE_HZ,
    RnnoiseFactory,
};

#[cfg(feature = "offline-wav")]
pub use offline::{OfflineDenoiser, OfflineError, denoise_latency_compensated};
