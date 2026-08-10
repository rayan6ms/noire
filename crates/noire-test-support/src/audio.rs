//! Deterministic audio-backend fake and reusable mono sample fixtures.

use std::{collections::VecDeque, error::Error, fmt};

use noire_core::audio::{AudioBackend, BackendCommand, BackendCommandError, BackendEvent};

/// A scripted callback-sized action for audio-pipeline tests.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum AudioCallbackStep {
    /// Present a capture buffer to the capture side of a test harness.
    Capture(FakeAudioBuffer),
    /// Ask the source side of a test harness to render exactly `frames` samples.
    Source {
        /// Requested mono frame count.
        frames: usize,
    },
}

/// Owned mono data plus independently scripted backend metadata.
///
/// Keeping the declared offset and frame count separate permits deterministic
/// malformed-buffer tests without invalid memory access.
#[derive(Clone, Debug, PartialEq)]
pub struct FakeAudioBuffer {
    samples: Vec<f32>,
    offset_frames: usize,
    declared_frames: usize,
}

impl FakeAudioBuffer {
    /// Creates a buffer whose metadata exactly covers its samples.
    #[must_use]
    pub fn valid(samples: Vec<f32>) -> Self {
        let declared_frames = samples.len();
        Self {
            samples,
            offset_frames: 0,
            declared_frames,
        }
    }

    /// Creates a buffer with independent offset and frame-count metadata.
    ///
    /// This is useful for testing bounds validation. The constructor itself
    /// never reads beyond `samples`.
    #[must_use]
    pub fn with_metadata(samples: Vec<f32>, offset_frames: usize, declared_frames: usize) -> Self {
        Self {
            samples,
            offset_frames,
            declared_frames,
        }
    }

    /// Returns the complete accessible backing region.
    #[must_use]
    pub fn backing_samples(&self) -> &[f32] {
        &self.samples
    }

    /// Returns the frame offset claimed by the scripted backend metadata.
    #[must_use]
    pub const fn offset_frames(&self) -> usize {
        self.offset_frames
    }

    /// Returns the frame count claimed by the scripted backend metadata.
    #[must_use]
    pub const fn declared_frames(&self) -> usize {
        self.declared_frames
    }

    /// Returns the declared region when its metadata is in bounds.
    #[must_use]
    pub fn declared_samples(&self) -> Option<&[f32]> {
        let end = self.offset_frames.checked_add(self.declared_frames)?;
        self.samples.get(self.offset_frames..end)
    }

    /// Reports whether the declared mono region is accessible.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.declared_samples().is_some()
    }
}

/// Deterministic control-plane backend with separate callback and output records.
///
/// Request outcomes, events, and callback steps use FIFO order. Requests default
/// to acceptance when no outcome is scripted. This fake performs no DSP and
/// applies no failure policy; tests drive the production policy around it.
#[derive(Debug, Default)]
pub struct FakeAudioBackend {
    request_outcomes: VecDeque<Result<(), BackendCommandError>>,
    requests: Vec<BackendCommand>,
    events: VecDeque<BackendEvent>,
    callback_steps: VecDeque<AudioCallbackStep>,
    published_blocks: Vec<Vec<f32>>,
}

impl FakeAudioBackend {
    /// Creates an empty fake that accepts requests by default.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Scripts the result of the next control request.
    pub fn script_request_outcome(&mut self, outcome: Result<(), BackendCommandError>) {
        self.request_outcomes.push_back(outcome);
    }

    /// Queues a low-rate backend event.
    pub fn script_event(&mut self, event: BackendEvent) {
        self.events.push_back(event);
    }

    /// Queues one capture or source callback action.
    pub fn script_callback(&mut self, step: AudioCallbackStep) {
        self.callback_steps.push_back(step);
    }

    /// Returns every requested command, including rejected attempts.
    #[must_use]
    pub fn requests(&self) -> &[BackendCommand] {
        &self.requests
    }

    /// Removes and returns the next scripted callback action.
    pub fn next_callback(&mut self) -> Option<AudioCallbackStep> {
        self.callback_steps.pop_front()
    }

    /// Records one source block produced by the system under test.
    pub fn record_published_block(&mut self, samples: Vec<f32>) {
        self.published_blocks.push(samples);
    }

    /// Returns source blocks recorded in publication order.
    #[must_use]
    pub fn published_blocks(&self) -> &[Vec<f32>] {
        &self.published_blocks
    }
}

impl AudioBackend for FakeAudioBackend {
    fn request(&mut self, command: BackendCommand) -> Result<(), BackendCommandError> {
        self.requests.push(command);
        self.request_outcomes.pop_front().unwrap_or(Ok(()))
    }

    fn poll_event(&mut self) -> Option<BackendEvent> {
        self.events.pop_front()
    }
}

/// Error constructing a deterministic audio fixture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FixtureError {
    /// An impulse index did not identify a sample in the requested fixture.
    ImpulseOutOfBounds,
    /// A chunk plan contained a zero-sized chunk.
    ZeroSizedChunk,
    /// Chunk sizes overflowed `usize` or did not consume the fixture exactly.
    ChunkLengthMismatch,
}

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ImpulseOutOfBounds => formatter.write_str("impulse index is out of bounds"),
            Self::ZeroSizedChunk => formatter.write_str("audio chunks must not be empty"),
            Self::ChunkLengthMismatch => {
                formatter.write_str("chunk sizes must consume the fixture exactly")
            }
        }
    }
}

impl Error for FixtureError {}

/// Creates `frames` samples of digital silence.
#[must_use]
pub fn silence(frames: usize) -> Vec<f32> {
    vec![0.0; frames]
}

/// Creates `frames` samples with one constant value.
#[must_use]
pub fn constant(frames: usize, value: f32) -> Vec<f32> {
    vec![value; frames]
}

/// Creates a silent fixture with one impulse.
///
/// # Errors
///
/// Returns [`FixtureError::ImpulseOutOfBounds`] when `index >= frames`.
pub fn impulse(frames: usize, index: usize, amplitude: f32) -> Result<Vec<f32>, FixtureError> {
    let mut samples = silence(frames);
    let sample = samples
        .get_mut(index)
        .ok_or(FixtureError::ImpulseOutOfBounds)?;
    *sample = amplitude;
    Ok(samples)
}

/// Splits a fixture according to an exact callback-size plan.
///
/// # Errors
///
/// Returns [`FixtureError::ZeroSizedChunk`] for a zero size, or
/// [`FixtureError::ChunkLengthMismatch`] when sizes overflow or do not consume
/// `samples` exactly.
pub fn split_exact(samples: &[f32], chunk_sizes: &[usize]) -> Result<Vec<Vec<f32>>, FixtureError> {
    let mut chunks = Vec::with_capacity(chunk_sizes.len());
    let mut start = 0_usize;

    for &size in chunk_sizes {
        if size == 0 {
            return Err(FixtureError::ZeroSizedChunk);
        }

        let end = start
            .checked_add(size)
            .ok_or(FixtureError::ChunkLengthMismatch)?;
        let chunk = samples
            .get(start..end)
            .ok_or(FixtureError::ChunkLengthMismatch)?;
        chunks.push(chunk.to_vec());
        start = end;
    }

    if start != samples.len() {
        return Err(FixtureError::ChunkLengthMismatch);
    }

    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use noire_core::audio::{
        AudioBackend, BackendCommand, BackendCommandError, BackendEvent, BackendFault,
    };

    use super::{
        AudioCallbackStep, FakeAudioBackend, FakeAudioBuffer, FixtureError, constant, impulse,
        silence, split_exact,
    };

    #[test]
    fn backend_preserves_script_and_request_order() {
        let mut backend = FakeAudioBackend::new();
        backend.script_request_outcome(Err(BackendCommandError::Busy));
        backend.script_event(BackendEvent::Fault(BackendFault::Disconnected));
        backend.script_event(BackendEvent::Reset { generation: 7 });

        assert_eq!(
            backend.request(BackendCommand::Start),
            Err(BackendCommandError::Busy)
        );
        assert_eq!(backend.request(BackendCommand::Stop), Ok(()));
        assert_eq!(
            backend.request(BackendCommand::Reset { generation: 7 }),
            Ok(())
        );
        assert_eq!(
            backend.requests(),
            &[
                BackendCommand::Start,
                BackendCommand::Stop,
                BackendCommand::Reset { generation: 7 }
            ]
        );
        assert_eq!(
            backend.poll_event(),
            Some(BackendEvent::Fault(BackendFault::Disconnected))
        );
        assert_eq!(
            backend.poll_event(),
            Some(BackendEvent::Reset { generation: 7 })
        );
        assert_eq!(backend.poll_event(), None);
    }

    #[test]
    fn backend_port_is_object_safe() {
        fn poll_once(backend: &mut dyn AudioBackend) -> Option<BackendEvent> {
            backend.poll_event()
        }

        let mut backend = FakeAudioBackend::new();
        backend.script_event(BackendEvent::Started);

        assert_eq!(poll_once(&mut backend), Some(BackendEvent::Started));
    }

    #[test]
    fn callback_script_supports_valid_and_malformed_capture() {
        let mut backend = FakeAudioBackend::new();
        backend.script_callback(AudioCallbackStep::Capture(FakeAudioBuffer::with_metadata(
            vec![9.0, 0.25, -0.25, 9.0],
            1,
            2,
        )));
        backend.script_callback(AudioCallbackStep::Capture(FakeAudioBuffer::with_metadata(
            vec![0.5],
            0,
            2,
        )));
        backend.script_callback(AudioCallbackStep::Source { frames: 128 });

        let first = backend.next_callback();
        let second = backend.next_callback();
        let third = backend.next_callback();

        assert!(matches!(first, Some(AudioCallbackStep::Capture(buffer))
            if buffer.declared_samples() == Some([0.25, -0.25].as_slice())));
        assert!(matches!(
            second,
            Some(AudioCallbackStep::Capture(buffer)) if !buffer.is_well_formed()
        ));
        assert_eq!(third, Some(AudioCallbackStep::Source { frames: 128 }));
        assert_eq!(backend.next_callback(), None);
    }

    #[test]
    fn lifecycle_reset_and_every_required_fault_are_scriptable() {
        let mut backend = FakeAudioBackend::new();
        let events = [
            BackendEvent::Started,
            BackendEvent::Fault(BackendFault::Disconnected),
            BackendEvent::Fault(BackendFault::MalformedBuffer),
            BackendEvent::Fault(BackendFault::Overflow),
            BackendEvent::Fault(BackendFault::Underflow),
            BackendEvent::Reset { generation: 11 },
            BackendEvent::Stopped,
        ];

        for event in events {
            backend.script_event(event);
        }
        for event in events {
            assert_eq!(backend.poll_event(), Some(event));
        }
        assert_eq!(backend.poll_event(), None);
    }

    #[test]
    fn published_blocks_are_retained_in_order() {
        let mut backend = FakeAudioBackend::new();
        backend.record_published_block(vec![0.1, 0.2]);
        backend.record_published_block(vec![0.3]);

        assert_eq!(backend.published_blocks(), &[vec![0.1, 0.2], vec![0.3]]);
    }

    #[test]
    fn fixtures_are_exact_and_deterministic() {
        assert_eq!(silence(3), vec![0.0, 0.0, 0.0]);
        assert_eq!(constant(2, -0.5), vec![-0.5, -0.5]);
        assert_eq!(impulse(4, 2, 1.0), Ok(vec![0.0, 0.0, 1.0, 0.0]));
        assert_eq!(impulse(4, 4, 1.0), Err(FixtureError::ImpulseOutOfBounds));
    }

    #[test]
    fn exact_split_preserves_samples_and_rejects_bad_plans() {
        let samples = vec![0.0, 1.0, 2.0, 3.0, 4.0];

        assert_eq!(
            split_exact(&samples, &[2, 1, 2]),
            Ok(vec![vec![0.0, 1.0], vec![2.0], vec![3.0, 4.0]])
        );
        assert_eq!(
            split_exact(&samples, &[2, 0, 3]),
            Err(FixtureError::ZeroSizedChunk)
        );
        assert_eq!(
            split_exact(&samples, &[2, 2]),
            Err(FixtureError::ChunkLengthMismatch)
        );
        assert_eq!(
            split_exact(&samples, &[usize::MAX, 6]),
            Err(FixtureError::ChunkLengthMismatch)
        );
    }
}

#[cfg(all(test, feature = "properties"))]
mod property_tests {
    use proptest::{collection, prelude::*};

    use super::split_exact;

    proptest! {
        #[test]
        fn exact_split_round_trips_arbitrary_finite_samples(
            samples in collection::vec(-1.0_f32..1.0_f32, 1..512),
            width in 1_usize..128,
        ) {
            let full_chunks = samples.len() / width;
            let remainder = samples.len() % width;
            let mut sizes = vec![width; full_chunks];
            if remainder != 0 {
                sizes.push(remainder);
            }

            match split_exact(&samples, &sizes) {
                Ok(chunks) => prop_assert_eq!(chunks.concat(), samples),
                Err(error) => prop_assert!(false, "unexpected fixture error: {error}"),
            }
        }
    }
}
