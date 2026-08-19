//! Minimal ownership-safe bindings to the vendored `FastEnhancer` C runtime.

use std::{ffi::c_void, ptr::NonNull};

const FE_MODEL_BASE: i32 = 1;

unsafe extern "C" {
    fn fe_init(model_size: i32, weight_data: *const u8, weight_len: i32) -> *mut c_void;
    fn fe_process(state: *mut c_void, input: *const f32, output: *mut f32) -> i32;
    fn fe_reset(state: *mut c_void);
    fn fe_destroy(state: *mut c_void);
    fn fe_get_hop_size(state: *const c_void) -> i32;
}

/// Errors produced by the native runtime boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// The weight artifact length cannot cross the C ABI.
    WeightArtifactTooLarge,
    /// `FastEnhancer` rejected the weight artifact or could not allocate state.
    InitializationFailed,
    /// The compiled runtime does not expose the expected 512-sample hop.
    IncompatibleRuntime,
    /// Native frame processing reported an internal error.
    ProcessingFailed,
}

/// One independently owned `FastEnhancer` Base 48 kHz state.
pub struct State {
    raw: NonNull<c_void>,
}

// The state contains no thread-affine resources. Exclusive processing is
// enforced by `&mut self`, and it is never shared between audio callbacks.
unsafe impl Send for State {}

impl State {
    /// Parses weights and creates a fresh recurrent state.
    ///
    /// # Errors
    ///
    /// Returns an error when the artifact or compiled runtime is incompatible.
    pub fn new(weights: &[u8]) -> Result<Self, Error> {
        let length = i32::try_from(weights.len()).map_err(|_| Error::WeightArtifactTooLarge)?;
        // SAFETY: `weights` is valid for `length` bytes for the duration of the
        // call. The C implementation validates and copies the full artifact.
        let raw = unsafe { fe_init(FE_MODEL_BASE, weights.as_ptr(), length) };
        let state = Self {
            raw: NonNull::new(raw).ok_or(Error::InitializationFailed)?,
        };
        // SAFETY: `raw` is a live state owned by `state`.
        let hop_size = unsafe { fe_get_hop_size(state.raw.as_ptr()) };
        if hop_size != 512 {
            return Err(Error::IncompatibleRuntime);
        }
        Ok(state)
    }

    /// Processes one exact 512-sample hop without allocating.
    ///
    /// # Errors
    ///
    /// Returns an error if native inference rejects the call.
    pub fn process(&mut self, input: &[f32; 512], output: &mut [f32; 512]) -> Result<(), Error> {
        // SAFETY: the state is live and exclusively borrowed; both arrays are
        // valid, non-overlapping 512-element buffers as required by the C API.
        let result = unsafe { fe_process(self.raw.as_ptr(), input.as_ptr(), output.as_mut_ptr()) };
        if result == 0 {
            Ok(())
        } else {
            output.fill(0.0);
            Err(Error::ProcessingFailed)
        }
    }

    /// Clears recurrent and overlap state while preserving weights.
    pub fn reset(&mut self) {
        // SAFETY: the state is live and exclusively borrowed.
        unsafe { fe_reset(self.raw.as_ptr()) };
    }
}

impl Drop for State {
    fn drop(&mut self) {
        // SAFETY: `raw` is owned by this value and destroyed exactly once.
        unsafe { fe_destroy(self.raw.as_ptr()) };
    }
}
