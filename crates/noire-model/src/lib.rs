//! Real-time denoising model contracts for Noire.
//!
//! Model construction and reset are deactivated/control-plane operations.
//! Exact-frame processing and descriptor access are synchronous and
//! allocation-free contracts suitable for Noire's audio callback.

#![forbid(unsafe_code)]

mod descriptor;
mod error;
mod frame;

pub use descriptor::{DescriptorError, ModelDescriptor, ModelDescriptorSpec};
pub use error::{CreateError, ProcessError};
pub use frame::{FrameStats, finalize_process_output, prepare_process_frame};

/// A synchronous exact-frame denoising model.
///
/// Implementations must not allocate, lock, wait, perform I/O, or log from
/// [`Self::descriptor`] or [`Self::process_frame`].
pub trait Denoiser: Send {
    /// Returns immutable metadata for this instance.
    ///
    /// The descriptor must remain unchanged for the lifetime of the instance.
    fn descriptor(&self) -> &ModelDescriptor;

    /// Restores recurrent state to the state of a newly created instance.
    ///
    /// Configuration and the descriptor remain unchanged. Reset is synchronous
    /// and safe to call after any processing error, but an adapter may rebuild
    /// owned model state. Callers must invoke it only while frame processing is
    /// deactivated and outside the audio callback.
    fn reset(&mut self);

    /// Processes exactly one frame into a distinct output buffer.
    ///
    /// The input and output lengths must both equal
    /// [`ModelDescriptor::frame_buffer_samples`]. On success, every output sample
    /// must be finite and either normal or zero, and returned statistics must be
    /// valid. On error, the implementation must leave the entire supplied output
    /// buffer as silence; the caller must reset the model before reuse.
    ///
    /// [`prepare_process_frame`] and [`finalize_process_output`] implement the
    /// shared boundary checks without allocating.
    ///
    /// # Errors
    ///
    /// Returns a bounded [`ProcessError`] for invalid frame shapes or values,
    /// invalid model output/statistics, or an internal model failure.
    fn process_frame(
        &mut self,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<FrameStats, ProcessError>;
}

/// A control-plane factory for denoiser instances.
///
/// Factories are object-safe so daemon composition does not depend on a concrete
/// adapter. Creation and destruction may allocate and must never run in an audio
/// callback.
pub trait DenoiserFactory: Send + Sync {
    /// Returns the immutable descriptor for instances this factory creates.
    fn descriptor(&self) -> &ModelDescriptor;

    /// Creates an independent model instance outside the real-time path.
    ///
    /// A successful instance must return a descriptor equal to the factory
    /// descriptor.
    ///
    /// # Errors
    ///
    /// Returns a bounded [`CreateError`] when model data is unavailable or
    /// invalid, or initialization fails.
    fn create(&self) -> Result<Box<dyn Denoiser>, CreateError>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use std::error::Error;

    use super::{
        CreateError, Denoiser, DenoiserFactory, DescriptorError, FrameStats, ModelDescriptor,
        ModelDescriptorSpec, ProcessError, finalize_process_output, prepare_process_frame,
    };

    const FRAME_SAMPLES: usize = 4;

    struct FakeDenoiser {
        descriptor: ModelDescriptor,
        recurrent_offset: f32,
    }

    impl Denoiser for FakeDenoiser {
        fn descriptor(&self) -> &ModelDescriptor {
            &self.descriptor
        }

        fn reset(&mut self) {
            self.recurrent_offset = 0.0;
        }

        fn process_frame(
            &mut self,
            input: &[f32],
            output: &mut [f32],
        ) -> Result<FrameStats, ProcessError> {
            prepare_process_frame(&self.descriptor, input, output)?;
            for (source, destination) in input.iter().zip(output.iter_mut()) {
                *destination = *source + self.recurrent_offset;
            }
            self.recurrent_offset += 0.125;
            finalize_process_output(output, FrameStats::new(0.75)?)
        }
    }

    struct FakeFactory {
        descriptor: ModelDescriptor,
    }

    impl DenoiserFactory for FakeFactory {
        fn descriptor(&self) -> &ModelDescriptor {
            &self.descriptor
        }

        fn create(&self) -> Result<Box<dyn Denoiser>, CreateError> {
            Ok(Box::new(FakeDenoiser {
                descriptor: self.descriptor,
                recurrent_offset: 0.0,
            }))
        }
    }

    fn descriptor() -> Result<ModelDescriptor, DescriptorError> {
        ModelDescriptor::new(ModelDescriptorSpec {
            id: "test.fake",
            name: "Deterministic fake",
            version: "1",
            license: "MIT",
            sample_rate_hz: 48_000,
            channels: 1,
            frame_samples: FRAME_SAMPLES,
            hop_samples: FRAME_SAMPLES,
            lookahead_samples: 1,
            delay_samples: 2,
        })
    }

    #[test]
    fn descriptor_exposes_validated_contract_metadata() -> Result<(), DescriptorError> {
        let descriptor = descriptor()?;
        assert_eq!(descriptor.id(), "test.fake");
        assert_eq!(descriptor.name(), "Deterministic fake");
        assert_eq!(descriptor.version(), "1");
        assert_eq!(descriptor.license(), "MIT");
        assert_eq!(descriptor.sample_rate_hz(), 48_000);
        assert_eq!(descriptor.channels(), 1);
        assert_eq!(descriptor.frame_samples(), FRAME_SAMPLES);
        assert_eq!(descriptor.hop_samples(), FRAME_SAMPLES);
        assert_eq!(descriptor.lookahead_samples(), 1);
        assert_eq!(descriptor.delay_samples(), 2);
        assert_eq!(descriptor.frame_buffer_samples(), FRAME_SAMPLES);
        Ok(())
    }

    #[test]
    fn factory_and_denoiser_are_object_safe_and_descriptors_match() -> Result<(), Box<dyn Error>> {
        let factory = FakeFactory {
            descriptor: descriptor()?,
        };
        let erased_factory: &dyn DenoiserFactory = &factory;
        let model: Box<dyn Denoiser> = erased_factory.create()?;
        assert_eq!(model.descriptor(), erased_factory.descriptor());
        Ok(())
    }

    #[test]
    fn reset_reproduces_new_instance_state() -> Result<(), Box<dyn Error>> {
        let factory = FakeFactory {
            descriptor: descriptor()?,
        };
        let mut model = factory.create()?;
        let input = [0.1, 0.2, 0.3, 0.4];
        let mut first = [0.0; FRAME_SAMPLES];
        let mut changed = [0.0; FRAME_SAMPLES];
        let mut after_reset = [0.0; FRAME_SAMPLES];

        model.process_frame(&input, &mut first)?;
        model.process_frame(&input, &mut changed)?;
        model.reset();
        model.process_frame(&input, &mut after_reset)?;

        assert_ne!(changed, first);
        assert_eq!(after_reset, first);
        Ok(())
    }

    #[test]
    fn malformed_frame_fails_closed_and_recovers_after_reset() -> Result<(), Box<dyn Error>> {
        let factory = FakeFactory {
            descriptor: descriptor()?,
        };
        let mut model = factory.create()?;
        let mut output = [1.0; FRAME_SAMPLES];
        let error = model.process_frame(&[0.0, f32::NAN, 0.0, 0.0], &mut output);
        assert_eq!(error, Err(ProcessError::NonFiniteInput));
        assert_eq!(output, [0.0; FRAME_SAMPLES]);

        model.reset();
        let mut valid = [0.0; FRAME_SAMPLES];
        model.process_frame(&[0.0; FRAME_SAMPLES], &mut valid)?;
        assert_eq!(valid, [0.0; FRAME_SAMPLES]);
        Ok(())
    }

    #[test]
    fn contract_errors_are_copy_and_machine_sized() {
        const fn assert_copy<T: Copy>() {}
        assert_copy::<ProcessError>();
        assert_copy::<CreateError>();
        assert!(!std::mem::needs_drop::<ProcessError>());
        assert!(!std::mem::needs_drop::<CreateError>());
        assert!(size_of::<ProcessError>() <= size_of::<usize>());
        assert!(size_of::<CreateError>() <= size_of::<usize>());
    }
}
