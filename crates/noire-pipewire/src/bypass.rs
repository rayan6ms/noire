//! Bounded generation-aware SPSC transport for the latency-matched bypass path.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use noire_dsp::{
    FaultRamp, FaultRampError, FaultRampState, MAX_CALLBACK_FRAMES, MODEL_FRAME_SAMPLES,
};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::{CaptureSink, InputGeneration};

/// Fixed overload boundary from the architecture plan.
pub const BYPASS_RING_CAPACITY: usize = 2 * MAX_CALLBACK_FRAMES + 2 * MODEL_FRAME_SAMPLES;
/// Graph quanta retained beyond the model-frame delay to absorb stream jitter.
pub const BYPASS_STARTUP_QUANTA: usize = 3;

#[derive(Clone, Copy, Debug, Default)]
struct TaggedSample {
    generation: u64,
    value: f32,
}

#[derive(Debug)]
struct SharedState {
    generation: AtomicU64,
    produced_frames: AtomicU64,
    output_callbacks: AtomicU64,
    output_frames: AtomicU64,
    startup_silence_frames: AtomicU64,
    missing_frames: AtomicU64,
    underflows: AtomicU64,
    overflows: AtomicU64,
    dropped_frames: AtomicU64,
    discarded_stale_frames: AtomicU64,
    generation_resets: AtomicU64,
    oversized_requests: AtomicU64,
    sanitized_samples: AtomicU64,
    high_water_frames: AtomicU64,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            generation: AtomicU64::new(1),
            produced_frames: AtomicU64::new(0),
            output_callbacks: AtomicU64::new(0),
            output_frames: AtomicU64::new(0),
            startup_silence_frames: AtomicU64::new(0),
            missing_frames: AtomicU64::new(0),
            underflows: AtomicU64::new(0),
            overflows: AtomicU64::new(0),
            dropped_frames: AtomicU64::new(0),
            discarded_stale_frames: AtomicU64::new(0),
            generation_resets: AtomicU64::new(0),
            oversized_requests: AtomicU64::new(0),
            sanitized_samples: AtomicU64::new(0),
            high_water_frames: AtomicU64::new(0),
        }
    }
}

/// Lock-free transport metrics read by the control plane.
#[derive(Clone, Debug)]
pub struct BypassTelemetry {
    shared: Arc<SharedState>,
}

/// Immutable snapshot of bypass transport health.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BypassTelemetrySnapshot {
    /// Current resynchronization generation.
    pub generation: u64,
    /// Finite capture samples accepted by the producer.
    pub produced_frames: u64,
    /// Source process callback invocations.
    pub output_callbacks: u64,
    /// Frames written to source buffers, including deliberate silence.
    pub output_frames: u64,
    /// Leading-silence frames emitted while establishing exact startup occupancy.
    pub startup_silence_frames: u64,
    /// Frames synthesized by the fault ramp because fresh audio was unavailable.
    pub missing_frames: u64,
    /// Unexpected steady-state queue shortages.
    pub underflows: u64,
    /// Producer writes rejected to preserve the fixed latency bound.
    pub overflows: u64,
    /// Newly captured frames dropped on overflow.
    pub dropped_frames: u64,
    /// Samples from superseded generations removed without publication.
    pub discarded_stale_frames: u64,
    /// Explicit or fault-triggered generation changes.
    pub generation_resets: u64,
    /// Source callbacks that requested more than the fixed maximum quantum.
    pub oversized_requests: u64,
    /// Invalid/subnormal samples suppressed by the output boundary.
    pub sanitized_samples: u64,
    /// Maximum observed queue occupancy.
    pub high_water_frames: u64,
}

impl BypassTelemetry {
    /// Reads all transport counters without acquiring a lock.
    #[must_use]
    pub fn snapshot(&self) -> BypassTelemetrySnapshot {
        let load = |value: &AtomicU64| value.load(Ordering::Relaxed);
        BypassTelemetrySnapshot {
            generation: self.shared.generation.load(Ordering::Acquire),
            produced_frames: load(&self.shared.produced_frames),
            output_callbacks: load(&self.shared.output_callbacks),
            output_frames: load(&self.shared.output_frames),
            startup_silence_frames: load(&self.shared.startup_silence_frames),
            missing_frames: load(&self.shared.missing_frames),
            underflows: load(&self.shared.underflows),
            overflows: load(&self.shared.overflows),
            dropped_frames: load(&self.shared.dropped_frames),
            discarded_stale_frames: load(&self.shared.discarded_stale_frames),
            generation_resets: load(&self.shared.generation_resets),
            oversized_requests: load(&self.shared.oversized_requests),
            sanitized_samples: load(&self.shared.sanitized_samples),
            high_water_frames: load(&self.shared.high_water_frames),
        }
    }
}

/// Control-plane handle for starting a fresh audio generation.
#[derive(Clone, Debug)]
pub struct BypassControl {
    shared: Arc<SharedState>,
}

impl BypassControl {
    /// Invalidates queued samples and returns the new monotonic generation.
    #[must_use]
    pub fn request_resync(&self) -> u64 {
        self.shared
            .generation_resets
            .fetch_add(1, Ordering::Relaxed);
        self.shared
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1)
    }

    /// Returns the currently requested generation.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }
}

/// Capture-callback owner of the SPSC producer.
#[derive(Debug)]
pub struct BypassCaptureSink {
    producer: Producer<TaggedSample>,
    control: BypassControl,
}

impl CaptureSink for BypassCaptureSink {
    fn reset(&mut self, _generation: InputGeneration) {
        let _ = self.control.request_resync();
    }

    fn write(&mut self, _generation: InputGeneration, samples: &[f32]) {
        let generation = self.control.generation();
        if self.producer.slots() < samples.len() {
            self.control
                .shared
                .overflows
                .fetch_add(1, Ordering::Relaxed);
            self.control.shared.dropped_frames.fetch_add(
                u64::try_from(samples.len()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
            let _ = self.control.request_resync();
            return;
        }

        for sample in samples {
            // The all-or-nothing capacity check above makes failure impossible for
            // this single producer; retaining the branch keeps the overload policy
            // safe if the underlying implementation changes.
            if self
                .producer
                .push(TaggedSample {
                    generation,
                    value: *sample,
                })
                .is_err()
            {
                self.control
                    .shared
                    .overflows
                    .fetch_add(1, Ordering::Relaxed);
                self.control.shared.dropped_frames.fetch_add(
                    u64::try_from(samples.len()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
                let _ = self.control.request_resync();
                return;
            }
        }
        self.control.shared.produced_frames.fetch_add(
            u64::try_from(samples.len()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.control.shared.high_water_frames.fetch_max(
            u64::try_from(self.producer.buffer().capacity() - self.producer.slots())
                .unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}

/// Failure returned after publishing safe silence for an invalid source request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BypassOutputError {
    /// The graph quantum exceeds the fixed callback boundary.
    QuantumTooLarge,
    /// The shared fault-ramp primitive rejected an internal shape.
    FaultRamp(FaultRampError),
}

impl std::fmt::Display for BypassOutputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QuantumTooLarge => formatter.write_str("source quantum exceeds fixed boundary"),
            Self::FaultRamp(error) => write!(formatter, "source fault ramp failed: {error}"),
        }
    }
}

impl std::error::Error for BypassOutputError {}

/// Outcome of one bounded source fill.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BypassOutputReport {
    /// Frames written to the destination.
    pub frames: u16,
    /// Missing frames synthesized by the fault policy.
    pub missing_frames: u16,
    /// Whether this callback was part of the declared startup region.
    pub startup: bool,
    /// Ramp state after the callback.
    pub ramp_state: FaultRampState,
}

/// Source-callback owner of the SPSC consumer and fixed scratch storage.
#[derive(Debug)]
pub struct BypassOutput {
    consumer: Consumer<TaggedSample>,
    control: BypassControl,
    generation: u64,
    startup_ready: bool,
    scratch: [f32; MAX_CALLBACK_FRAMES],
    ramp: FaultRamp,
}

impl BypassOutput {
    /// Records an oversized graph request rejected before a typed destination
    /// slice can be formed.
    #[cfg(feature = "pipewire-backend")]
    pub(crate) fn reject_oversized_request(&mut self, frames: usize) {
        self.control
            .shared
            .output_callbacks
            .fetch_add(1, Ordering::Relaxed);
        self.control
            .shared
            .output_frames
            .fetch_add(u64::try_from(frames).unwrap_or(u64::MAX), Ordering::Relaxed);
        self.control
            .shared
            .oversized_requests
            .fetch_add(1, Ordering::Relaxed);
        self.ramp.reset_silent();
    }

    /// Fills exactly one source callback or publishes bounded safe silence.
    ///
    /// # Errors
    ///
    /// Oversized requests and internal ramp shape failures are reported after
    /// clearing the destination.
    pub fn fill(&mut self, output: &mut [f32]) -> Result<BypassOutputReport, BypassOutputError> {
        self.control
            .shared
            .output_callbacks
            .fetch_add(1, Ordering::Relaxed);
        self.control.shared.output_frames.fetch_add(
            u64::try_from(output.len()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        if output.len() > MAX_CALLBACK_FRAMES {
            output.fill(0.0);
            self.ramp.reset_silent();
            self.control
                .shared
                .oversized_requests
                .fetch_add(1, Ordering::Relaxed);
            return Err(BypassOutputError::QuantumTooLarge);
        }

        self.synchronize_generation();
        self.discard_stale_prefix();
        let startup_required =
            MODEL_FRAME_SAMPLES.saturating_add(output.len().saturating_mul(BYPASS_STARTUP_QUANTA));
        if !self.startup_ready && self.consumer.slots() < startup_required {
            output.fill(0.0);
            self.control.shared.startup_silence_frames.fetch_add(
                u64::try_from(output.len()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
            return Ok(BypassOutputReport {
                frames: u16::try_from(output.len()).unwrap_or(u16::MAX),
                startup: true,
                ramp_state: self.ramp.state(),
                ..BypassOutputReport::default()
            });
        }
        if !self.startup_ready {
            self.startup_ready = true;
            self.ramp.begin_recovery();
        }

        let mut complete = true;
        for destination in &mut self.scratch[..output.len()] {
            match self.consumer.pop() {
                Ok(sample) if sample.generation == self.generation => {
                    *destination = sample.value;
                }
                Ok(_) | Err(_) => {
                    complete = false;
                    break;
                }
            }
        }

        let ramp_report = if complete {
            self.ramp
                .process(Some(&self.scratch[..output.len()]), output)
        } else {
            self.control
                .shared
                .underflows
                .fetch_add(1, Ordering::Relaxed);
            let _ = self.control.request_resync();
            self.startup_ready = false;
            self.ramp.process(None, output)
        }
        .map_err(BypassOutputError::FaultRamp)?;
        self.control
            .shared
            .missing_frames
            .fetch_add(u64::from(ramp_report.missing_samples), Ordering::Relaxed);
        self.control.shared.sanitized_samples.fetch_add(
            ramp_report
                .sanitized
                .non_finite
                .saturating_add(ramp_report.sanitized.subnormal),
            Ordering::Relaxed,
        );
        Ok(BypassOutputReport {
            frames: u16::try_from(output.len()).unwrap_or(u16::MAX),
            missing_frames: ramp_report.missing_samples,
            startup: false,
            ramp_state: ramp_report.state,
        })
    }

    /// Removes all queued audio and returns immediately to startup silence.
    pub fn clear_sensitive(&mut self) {
        let _ = self.control.request_resync();
        self.discard_pending_sensitive();
        self.generation = self.control.generation();
    }

    /// Clears queued and scratch audio while retaining the current generation.
    ///
    /// The control loop uses this while a consumer disconnect is inside its
    /// debounce window: capture remains warm, but the paused source cannot let
    /// sensitive samples accumulate or overflow the bounded ring.
    pub fn discard_pending_sensitive(&mut self) {
        while self.consumer.pop().is_ok() {
            self.control
                .shared
                .discarded_stale_frames
                .fetch_add(1, Ordering::Relaxed);
        }
        self.scratch.fill(0.0);
        self.startup_ready = false;
        self.ramp.reset_silent();
    }

    fn synchronize_generation(&mut self) {
        let requested = self.control.generation();
        if requested != self.generation {
            self.generation = requested;
            self.startup_ready = false;
            self.ramp.reset_silent();
            self.scratch.fill(0.0);
        }
    }

    fn discard_stale_prefix(&mut self) {
        while self
            .consumer
            .peek()
            .is_ok_and(|sample| sample.generation != self.generation)
        {
            let _ = self.consumer.pop();
            self.control
                .shared
                .discarded_stale_frames
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Allocates the fixed ring before activation and returns its owned endpoints.
#[must_use]
pub fn create_bypass_channel() -> (
    BypassCaptureSink,
    BypassOutput,
    BypassControl,
    BypassTelemetry,
) {
    let (producer, consumer) = RingBuffer::new(BYPASS_RING_CAPACITY);
    let shared = Arc::new(SharedState::default());
    let control = BypassControl {
        shared: Arc::clone(&shared),
    };
    (
        BypassCaptureSink {
            producer,
            control: control.clone(),
        },
        BypassOutput {
            consumer,
            control: control.clone(),
            generation: control.generation(),
            startup_ready: false,
            scratch: [0.0; MAX_CALLBACK_FRAMES],
            ramp: {
                let mut ramp = FaultRamp::new();
                ramp.reset_silent();
                ramp
            },
        },
        control,
        BypassTelemetry { shared },
    )
}

#[cfg(test)]
mod tests {
    use noire_dsp::{FAULT_RAMP_SAMPLES, MODEL_FRAME_SAMPLES};

    use super::{BYPASS_RING_CAPACITY, BYPASS_STARTUP_QUANTA, create_bypass_channel};
    use crate::{CaptureSink, InputGeneration};

    #[test]
    fn startup_retains_exact_model_lead_and_fades_in() -> Result<(), super::BypassOutputError> {
        let (mut producer, mut output, _control, telemetry) = create_bypass_channel();
        let quantum = 128;
        let startup_samples = MODEL_FRAME_SAMPLES + BYPASS_STARTUP_QUANTA * quantum;
        let input = vec![0.5; startup_samples];
        let mut destination = [1.0; 128];

        producer.write(InputGeneration::INITIAL, &input[..input.len() - 1]);
        let startup = output.fill(&mut destination)?;
        assert!(startup.startup);
        assert!(
            destination
                .iter()
                .all(|sample| sample.abs() <= f32::EPSILON)
        );

        producer.write(InputGeneration::INITIAL, &input[input.len() - 1..]);
        let recovered = output.fill(&mut destination)?;
        assert!(!recovered.startup);
        assert!(destination[0] > 0.0);
        assert!(destination[0] < 0.5);
        assert!(destination[127] <= 0.5);
        assert_eq!(telemetry.snapshot().underflows, 0);
        Ok(())
    }

    #[test]
    fn steady_underflow_never_replays_stale_audio() -> Result<(), super::BypassOutputError> {
        let (mut producer, mut output, _control, telemetry) = create_bypass_channel();
        let input = vec![0.75; MODEL_FRAME_SAMPLES + BYPASS_STARTUP_QUANTA * 64];
        producer.write(InputGeneration::INITIAL, &input);
        let mut destination = [0.0; 64];
        output.fill(&mut destination)?;
        for _ in 0..input.len().div_ceil(64) {
            output.fill(&mut destination)?;
        }
        assert_eq!(telemetry.snapshot().underflows, 1);

        for _ in 0..usize::from(FAULT_RAMP_SAMPLES).div_ceil(64) {
            output.fill(&mut destination)?;
        }
        assert!(
            destination
                .iter()
                .all(|sample| sample.abs() <= f32::EPSILON)
        );
        Ok(())
    }

    #[test]
    fn overflow_drops_new_audio_and_requests_bounded_resync() -> Result<(), super::BypassOutputError>
    {
        let (mut producer, mut output, control, telemetry) = create_bypass_channel();
        let block = [0.25; 4_096];
        producer.write(InputGeneration::INITIAL, &block);
        producer.write(InputGeneration::INITIAL, &block);
        producer.write(InputGeneration::INITIAL, &block);
        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.overflows, 1);
        assert_eq!(snapshot.dropped_frames, 4_096);
        assert_eq!(control.generation(), 2);
        assert!(snapshot.high_water_frames <= BYPASS_RING_CAPACITY as u64);

        let mut destination = [1.0; 128];
        let report = output.fill(&mut destination)?;
        assert!(report.startup);
        assert!(
            destination
                .iter()
                .all(|sample| sample.abs() <= f32::EPSILON)
        );
        Ok(())
    }

    #[test]
    fn explicit_clear_removes_sensitive_samples() -> Result<(), super::BypassOutputError> {
        let (mut producer, mut output, control, telemetry) = create_bypass_channel();
        producer.write(InputGeneration::INITIAL, &[0.9; 1_024]);
        let before = control.generation();
        output.clear_sensitive();
        assert!(control.generation() > before);
        assert_eq!(telemetry.snapshot().discarded_stale_frames, 1_024);
        let mut destination = [1.0; 128];
        assert!(output.fill(&mut destination)?.startup);
        assert!(
            destination
                .iter()
                .all(|sample| sample.abs() <= f32::EPSILON)
        );
        Ok(())
    }

    #[test]
    fn debounce_drain_clears_audio_without_advancing_generation() {
        let (mut producer, mut output, control, telemetry) = create_bypass_channel();
        producer.write(InputGeneration::INITIAL, &[0.7; 1_024]);
        let generation = control.generation();

        output.discard_pending_sensitive();

        assert_eq!(control.generation(), generation);
        assert_eq!(telemetry.snapshot().discarded_stale_frames, 1_024);
        producer.write(InputGeneration::INITIAL, &[0.3; 128]);
        assert_eq!(telemetry.snapshot().overflows, 0);
    }
}
