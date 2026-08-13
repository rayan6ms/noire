//! Allocation-free live-model processing between capture and source transport.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use noire_dsp::{
    DcBlocker, DryDelay, DryDelayError, EqualPowerMixer, FrameAssembler, MODEL_FRAME_SAMPLES,
    Meter, MixReport, ModelFrame, SAMPLE_RATE_HZ, StrengthRamp,
};
use noire_model::{Denoiser, ProcessError};

use crate::{
    BypassCaptureSink, BypassOutput, BypassTelemetry, BypassTelemetrySnapshot, CaptureSink,
    InputGeneration, bypass::create_processed_channel,
};

const TIMING_BUCKETS: usize = 32;
const TIMING_BUCKET_WIDTH_NS: u64 = 50_000;
const DEFAULT_TIMING_SAMPLE_INTERVAL: u64 = 100;
const METER_PUBLISH_SAMPLES: u64 = 4_800;

/// Default model deadline and degradation window from the architecture plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlinePolicy {
    /// Maximum sampled model duration before a miss is recorded.
    pub model_deadline: Duration,
    /// Misses within one window that enter degraded-performance state.
    pub misses_per_window: u8,
    /// Rolling window for consecutive sampled misses.
    pub window: Duration,
}

impl Default for DeadlinePolicy {
    fn default() -> Self {
        Self {
            model_deadline: Duration::from_micros(750),
            misses_per_window: 5,
            window: Duration::from_secs(10),
        }
    }
}

/// Explicit behavior after a model failure.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u32)]
pub enum FailMode {
    /// Stop producing new audio so the source drains processed data and ramps to silence.
    #[default]
    Closed = 0,
    /// Publish latency-matched dry audio after the user explicitly opts in.
    Open = 1,
}

/// Current real-time model health.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u32)]
pub enum LiveState {
    /// Model processing is healthy.
    #[default]
    Running = 0,
    /// Five sampled deadline misses occurred inside the configured window.
    DegradedPerformance = 1,
    /// Model processing failed and the selected fail policy is active.
    ModelFailed = 2,
    /// The processed transport overflowed and requires a deactivated reset.
    TransportFailed = 3,
}

impl LiveState {
    fn from_raw(value: u32) -> Self {
        match value {
            1 => Self::DegradedPerformance,
            2 => Self::ModelFailed,
            3 => Self::TransportFailed,
            _ => Self::Running,
        }
    }
}

/// Construction failure for the canonical live model path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LivePipelineError {
    /// The injected model does not implement mono 48-kHz, 480-sample frames.
    IncompatibleModel,
    /// The declared model delay exceeds the bounded dry-delay storage.
    DryDelay(DryDelayError),
}

impl std::fmt::Display for LivePipelineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IncompatibleModel => {
                formatter.write_str("model is incompatible with canonical live audio")
            }
            Self::DryDelay(error) => write!(formatter, "could not construct dry delay: {error}"),
        }
    }
}

impl std::error::Error for LivePipelineError {}

#[repr(align(64))]
#[derive(Debug)]
struct AtomicParameters {
    epoch: AtomicU64,
    strength_bits: AtomicU32,
    enabled: AtomicBool,
    fail_mode: AtomicU32,
    diagnostic_timing: AtomicBool,
}

impl Default for AtomicParameters {
    fn default() -> Self {
        Self {
            epoch: AtomicU64::new(0),
            strength_bits: AtomicU32::new(1.0_f32.to_bits()),
            enabled: AtomicBool::new(true),
            fail_mode: AtomicU32::new(FailMode::Closed as u32),
            diagnostic_timing: AtomicBool::new(false),
        }
    }
}

#[derive(Debug)]
struct TimingHistogram {
    samples: AtomicU64,
    total_ns: AtomicU64,
    maximum_ns: AtomicU64,
    buckets: [AtomicU64; TIMING_BUCKETS],
}

impl Default for TimingHistogram {
    fn default() -> Self {
        Self {
            samples: AtomicU64::new(0),
            total_ns: AtomicU64::new(0),
            maximum_ns: AtomicU64::new(0),
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl TimingHistogram {
    fn observe(&self, elapsed: Duration) {
        let nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        self.samples.fetch_add(1, Ordering::Relaxed);
        self.total_ns.fetch_add(nanos, Ordering::Relaxed);
        self.maximum_ns.fetch_max(nanos, Ordering::Relaxed);
        let raw_bucket = nanos / TIMING_BUCKET_WIDTH_NS;
        let bucket = usize::try_from(raw_bucket)
            .unwrap_or(usize::MAX)
            .min(TIMING_BUCKETS - 1);
        self.buckets[bucket].fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> TimingHistogramSnapshot {
        TimingHistogramSnapshot {
            samples: self.samples.load(Ordering::Relaxed),
            total_ns: self.total_ns.load(Ordering::Relaxed),
            maximum_ns: self.maximum_ns.load(Ordering::Relaxed),
            buckets: std::array::from_fn(|index| self.buckets[index].load(Ordering::Relaxed)),
        }
    }
}

#[derive(Debug)]
struct SharedLiveState {
    parameters: AtomicParameters,
    state: AtomicU32,
    input_samples: AtomicU64,
    model_frames: AtomicU64,
    dry_frames: AtomicU64,
    model_errors: AtomicU64,
    model_resets: AtomicU64,
    deadline_misses: AtomicU64,
    hard_ceiling_samples: AtomicU64,
    sanitized_samples: AtomicU64,
    vad_bits: AtomicU32,
    peak_bits: AtomicU32,
    rms_bits: AtomicU32,
    model_timing: TimingHistogram,
    callback_timing: TimingHistogram,
}

impl Default for SharedLiveState {
    fn default() -> Self {
        Self {
            parameters: AtomicParameters::default(),
            state: AtomicU32::new(LiveState::Running as u32),
            input_samples: AtomicU64::new(0),
            model_frames: AtomicU64::new(0),
            dry_frames: AtomicU64::new(0),
            model_errors: AtomicU64::new(0),
            model_resets: AtomicU64::new(0),
            deadline_misses: AtomicU64::new(0),
            hard_ceiling_samples: AtomicU64::new(0),
            sanitized_samples: AtomicU64::new(0),
            vad_bits: AtomicU32::new(0.0_f32.to_bits()),
            peak_bits: AtomicU32::new(0.0_f32.to_bits()),
            rms_bits: AtomicU32::new(0.0_f32.to_bits()),
            model_timing: TimingHistogram::default(),
            callback_timing: TimingHistogram::default(),
        }
    }
}

/// Control-plane writer for frame-boundary parameters.
#[derive(Clone, Debug)]
pub struct LiveControl {
    shared: Arc<SharedLiveState>,
}

impl LiveControl {
    /// Sets the wet mix in `[0, 1]`; non-finite input selects safe dry strength zero.
    pub fn set_strength(&self, strength: f32) {
        self.update(|parameters| {
            let strength = if strength.is_finite() {
                strength.clamp(0.0, 1.0)
            } else {
                0.0
            };
            parameters
                .strength_bits
                .store(strength.to_bits(), Ordering::Relaxed);
        });
    }

    /// Enables or bypasses the model output without changing graph latency.
    pub fn set_enabled(&self, enabled: bool) {
        self.update(|parameters| parameters.enabled.store(enabled, Ordering::Relaxed));
    }

    /// Selects the explicit model-failure policy.
    pub fn set_fail_mode(&self, mode: FailMode) {
        self.update(|parameters| parameters.fail_mode.store(mode as u32, Ordering::Relaxed));
    }

    /// Samples every frame/callback for bounded diagnostic measurements.
    pub fn set_diagnostic_timing(&self, enabled: bool) {
        self.shared
            .parameters
            .diagnostic_timing
            .store(enabled, Ordering::Release);
    }

    fn update(&self, update: impl FnOnce(&AtomicParameters)) {
        self.shared.parameters.epoch.fetch_add(1, Ordering::AcqRel);
        update(&self.shared.parameters);
        self.shared.parameters.epoch.fetch_add(1, Ordering::Release);
    }
}

/// One fixed timing histogram copied from real-time atomics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TimingHistogramSnapshot {
    /// Sampled operations.
    pub samples: u64,
    /// Sum of sampled durations.
    pub total_ns: u64,
    /// Largest sampled duration.
    pub maximum_ns: u64,
    /// Fixed 50-microsecond buckets; the last bucket includes larger values.
    pub buckets: [u64; TIMING_BUCKETS],
}

impl TimingHistogramSnapshot {
    /// Returns the upper-bound nanoseconds for a percentile in `[0, 100]`.
    #[must_use]
    pub fn percentile_ns(self, percentile: u8) -> u64 {
        if self.samples == 0 {
            return 0;
        }
        let rank = self
            .samples
            .saturating_mul(u64::from(percentile.min(100)))
            .saturating_add(99)
            / 100;
        let mut cumulative = 0_u64;
        for (index, count) in self.buckets.into_iter().enumerate() {
            cumulative = cumulative.saturating_add(count);
            if cumulative >= rank.max(1) {
                return u64::try_from(index + 1)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(TIMING_BUCKET_WIDTH_NS);
            }
        }
        self.maximum_ns
    }
}

/// Lock-free control-plane view of live processing.
#[derive(Clone, Debug)]
pub struct LiveTelemetry {
    shared: Arc<SharedLiveState>,
    transport: BypassTelemetry,
}

/// Immutable live-processing and transport snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LiveTelemetrySnapshot {
    /// Current processing health.
    pub state: LiveState,
    /// Canonical capture samples accepted by the live sink.
    pub input_samples: u64,
    /// Exact frames passed successfully through the model.
    pub model_frames: u64,
    /// Exact frames emitted from explicit bypass/fail-open behavior.
    pub dry_frames: u64,
    /// Model contract or inference failures.
    pub model_errors: u64,
    /// Deactivated model resets.
    pub model_resets: u64,
    /// Sampled calls exceeding the model deadline.
    pub deadline_misses: u64,
    /// Samples constrained by the transparent output ceiling.
    pub hard_ceiling_samples: u64,
    /// Invalid/subnormal samples removed within live processing.
    pub sanitized_samples: u64,
    /// Most recent rate-limited voice probability.
    pub vad_probability: f32,
    /// Most recent rate-limited output peak.
    pub peak: f32,
    /// Most recent rate-limited output RMS.
    pub rms: f32,
    /// Sampled model execution distribution.
    pub model_timing: TimingHistogramSnapshot,
    /// Sampled complete sink-callback distribution.
    pub callback_timing: TimingHistogramSnapshot,
    /// Processed SPSC transport state.
    pub transport: BypassTelemetrySnapshot,
}

impl LiveTelemetry {
    /// Takes a consistent-enough lock-free diagnostics snapshot.
    #[must_use]
    pub fn snapshot(&self) -> LiveTelemetrySnapshot {
        let load = |value: &AtomicU64| value.load(Ordering::Relaxed);
        LiveTelemetrySnapshot {
            state: LiveState::from_raw(self.shared.state.load(Ordering::Acquire)),
            input_samples: load(&self.shared.input_samples),
            model_frames: load(&self.shared.model_frames),
            dry_frames: load(&self.shared.dry_frames),
            model_errors: load(&self.shared.model_errors),
            model_resets: load(&self.shared.model_resets),
            deadline_misses: load(&self.shared.deadline_misses),
            hard_ceiling_samples: load(&self.shared.hard_ceiling_samples),
            sanitized_samples: load(&self.shared.sanitized_samples),
            vad_probability: f32::from_bits(self.shared.vad_bits.load(Ordering::Relaxed)),
            peak: f32::from_bits(self.shared.peak_bits.load(Ordering::Relaxed)),
            rms: f32::from_bits(self.shared.rms_bits.load(Ordering::Relaxed)),
            model_timing: self.shared.model_timing.snapshot(),
            callback_timing: self.shared.callback_timing.snapshot(),
            transport: self.transport.snapshot(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ParameterSnapshot {
    epoch: u64,
    fail_mode: FailMode,
}

impl Default for ParameterSnapshot {
    fn default() -> Self {
        Self {
            epoch: 0,
            fail_mode: FailMode::Closed,
        }
    }
}

#[derive(Debug, Default)]
struct DeadlineWindow {
    misses: [Option<Instant>; 5],
    next: usize,
    count: usize,
}

impl DeadlineWindow {
    fn record(&mut self, now: Instant, policy: DeadlinePolicy) -> bool {
        let required = usize::from(policy.misses_per_window).clamp(1, self.misses.len());
        self.misses[self.next] = Some(now);
        self.next = (self.next + 1) % required;
        self.count = self.count.saturating_add(1).min(required);
        if self.count < required {
            return false;
        }
        self.misses[self.next].is_some_and(|oldest| now.duration_since(oldest) <= policy.window)
    }
}

/// Capture-callback owner of the model, frame state, and processed producer.
pub struct LiveCaptureSink {
    transport: BypassCaptureSink,
    model: Box<dyn Denoiser>,
    assembler: FrameAssembler,
    dc_blocker: DcBlocker,
    dry_delay: DryDelay,
    wet: ModelFrame,
    dry: ModelFrame,
    mixed: ModelFrame,
    callback_scratch: [f32; noire_dsp::MAX_CALLBACK_FRAMES],
    strength: StrengthRamp,
    parameters: ParameterSnapshot,
    meter: Meter,
    meter_samples: u64,
    pending_vad: f32,
    shared: Arc<SharedLiveState>,
    deadline_policy: DeadlinePolicy,
    deadline_window: DeadlineWindow,
    callback_count: u64,
    model_count: u64,
    failed: bool,
}

impl std::fmt::Debug for LiveCaptureSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LiveCaptureSink")
            .field("model", &self.model.descriptor())
            .field(
                "state",
                &LiveState::from_raw(self.shared.state.load(Ordering::Relaxed)),
            )
            .finish_non_exhaustive()
    }
}

impl LiveCaptureSink {
    fn new(
        transport: BypassCaptureSink,
        model: Box<dyn Denoiser>,
        shared: Arc<SharedLiveState>,
    ) -> Result<Self, LivePipelineError> {
        let descriptor = *model.descriptor();
        if descriptor.sample_rate_hz() != SAMPLE_RATE_HZ
            || descriptor.channels() != 1
            || descriptor.frame_samples() != MODEL_FRAME_SAMPLES
            || descriptor.hop_samples() != MODEL_FRAME_SAMPLES
        {
            return Err(LivePipelineError::IncompatibleModel);
        }
        let dry_delay =
            DryDelay::new(descriptor.delay_samples()).map_err(LivePipelineError::DryDelay)?;
        Ok(Self {
            transport,
            model,
            assembler: FrameAssembler::new(),
            dc_blocker: DcBlocker::new(),
            dry_delay,
            wet: [0.0; MODEL_FRAME_SAMPLES],
            dry: [0.0; MODEL_FRAME_SAMPLES],
            mixed: [0.0; MODEL_FRAME_SAMPLES],
            callback_scratch: [0.0; noire_dsp::MAX_CALLBACK_FRAMES],
            strength: StrengthRamp::new(1.0),
            parameters: ParameterSnapshot::default(),
            meter: Meter::new(),
            meter_samples: 0,
            pending_vad: 0.0,
            shared,
            deadline_policy: DeadlinePolicy::default(),
            deadline_window: DeadlineWindow::default(),
            callback_count: 0,
            model_count: 0,
            failed: false,
        })
    }

    /// Rebuilds recurrent and signal history while processing is deactivated.
    pub fn reset_deactivated(&mut self) {
        self.model.reset();
        self.reset_signal_state();
        self.shared.model_resets.fetch_add(1, Ordering::Relaxed);
    }

    /// Overrides the fixed deadline policy for deterministic fault tests.
    pub fn set_deadline_policy_deactivated(&mut self, policy: DeadlinePolicy) {
        self.deadline_policy = policy;
        self.deadline_window = DeadlineWindow::default();
    }

    fn reset_signal_state(&mut self) {
        self.assembler.reset();
        self.dc_blocker.reset();
        self.dry_delay.reset();
        self.wet.fill(0.0);
        self.dry.fill(0.0);
        self.mixed.fill(0.0);
        self.callback_scratch.fill(0.0);
        self.meter = Meter::new();
        self.meter_samples = 0;
        self.pending_vad = 0.0;
        self.deadline_window = DeadlineWindow::default();
        self.failed = false;
        self.shared
            .state
            .store(LiveState::Running as u32, Ordering::Release);
    }

    fn load_parameters(&mut self) {
        let first = self.shared.parameters.epoch.load(Ordering::Acquire);
        if first == self.parameters.epoch || !first.is_multiple_of(2) {
            return;
        }
        let strength = f32::from_bits(self.shared.parameters.strength_bits.load(Ordering::Relaxed));
        let enabled = self.shared.parameters.enabled.load(Ordering::Relaxed);
        let fail_mode =
            if self.shared.parameters.fail_mode.load(Ordering::Relaxed) == FailMode::Open as u32 {
                FailMode::Open
            } else {
                FailMode::Closed
            };
        let second = self.shared.parameters.epoch.load(Ordering::Acquire);
        if first == second {
            self.parameters = ParameterSnapshot {
                epoch: second,
                fail_mode,
            };
            let target = if enabled { strength } else { 0.0 };
            if target.to_bits() != self.strength.target().to_bits() {
                self.strength.set_target(target, 0);
            }
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn process_frame_parts(
        transport: &mut BypassCaptureSink,
        model: &mut Box<dyn Denoiser>,
        dry_delay: &mut DryDelay,
        wet: &mut ModelFrame,
        dry: &mut ModelFrame,
        mixed: &mut ModelFrame,
        strength: &mut StrengthRamp,
        parameters: ParameterSnapshot,
        meter: &mut Meter,
        meter_samples: &mut u64,
        pending_vad: &mut f32,
        shared: &SharedLiveState,
        deadline_policy: DeadlinePolicy,
        deadline_window: &mut DeadlineWindow,
        model_count: &mut u64,
        failed: &mut bool,
        frame: &ModelFrame,
    ) {
        let dry_report = dry_delay.process(frame, dry);
        let Ok(dry_report) = dry_report else {
            *failed = true;
            shared
                .state
                .store(LiveState::ModelFailed as u32, Ordering::Release);
            shared.model_errors.fetch_add(1, Ordering::Relaxed);
            return;
        };
        shared.sanitized_samples.fetch_add(
            dry_report.non_finite.saturating_add(dry_report.subnormal),
            Ordering::Relaxed,
        );

        if *failed {
            if parameters.fail_mode == FailMode::Open && transport.write_processed(dry) {
                shared.dry_frames.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }

        *model_count = model_count.saturating_add(1);
        let diagnostic = shared.parameters.diagnostic_timing.load(Ordering::Relaxed);
        let sample_timing =
            diagnostic || model_count.is_multiple_of(DEFAULT_TIMING_SAMPLE_INTERVAL);
        let started = sample_timing.then(Instant::now);
        let result = model.process_frame(frame, wet);
        if let Some(started) = started {
            let elapsed = started.elapsed();
            shared.model_timing.observe(elapsed);
            if elapsed > deadline_policy.model_deadline {
                shared.deadline_misses.fetch_add(1, Ordering::Relaxed);
                if deadline_window.record(Instant::now(), deadline_policy) {
                    shared
                        .state
                        .store(LiveState::DegradedPerformance as u32, Ordering::Release);
                }
            }
        }

        let stats = match result {
            Ok(stats) => stats,
            Err(
                ProcessError::InputFrameLength
                | ProcessError::OutputFrameLength
                | ProcessError::NonFiniteInput
                | ProcessError::NonFiniteOutput
                | ProcessError::InvalidStatistics
                | ProcessError::ModelFailure,
            ) => {
                wet.fill(0.0);
                *failed = true;
                shared.model_errors.fetch_add(1, Ordering::Relaxed);
                shared
                    .state
                    .store(LiveState::ModelFailed as u32, Ordering::Release);
                if parameters.fail_mode == FailMode::Open && transport.write_processed(dry) {
                    shared.dry_frames.fetch_add(1, Ordering::Relaxed);
                }
                return;
            }
        };
        *pending_vad = stats.vad_probability();

        let mut mix_report = MixReport::default();
        for ((dry_sample, wet_sample), destination) in
            dry.iter().zip(wet.iter()).zip(mixed.iter_mut())
        {
            *destination =
                EqualPowerMixer::mix(*dry_sample, *wet_sample, strength.next(), &mut mix_report);
        }
        shared
            .hard_ceiling_samples
            .fetch_add(mix_report.hard_ceiling, Ordering::Relaxed);
        shared.sanitized_samples.fetch_add(
            mix_report
                .sanitized
                .non_finite
                .saturating_add(mix_report.sanitized.subnormal),
            Ordering::Relaxed,
        );
        meter.observe(mixed);
        *meter_samples = meter_samples.saturating_add(MODEL_FRAME_SAMPLES as u64);
        if *meter_samples >= METER_PUBLISH_SAMPLES {
            let snapshot = meter.take_snapshot();
            shared
                .vad_bits
                .store(pending_vad.to_bits(), Ordering::Relaxed);
            shared
                .peak_bits
                .store(snapshot.peak.to_bits(), Ordering::Relaxed);
            shared
                .rms_bits
                .store(snapshot.rms.to_bits(), Ordering::Relaxed);
            *meter_samples = 0;
        }
        if transport.write_processed(mixed) {
            shared.model_frames.fetch_add(1, Ordering::Relaxed);
        } else {
            *failed = true;
            shared
                .state
                .store(LiveState::TransportFailed as u32, Ordering::Release);
        }
    }
}

impl CaptureSink for LiveCaptureSink {
    fn reset(&mut self, generation: InputGeneration) {
        self.transport.reset(generation);
        self.reset_deactivated();
    }

    fn write(&mut self, _generation: InputGeneration, samples: &[f32]) {
        self.callback_count = self.callback_count.saturating_add(1);
        let diagnostic = self
            .shared
            .parameters
            .diagnostic_timing
            .load(Ordering::Relaxed);
        let sample_timing = diagnostic
            || self
                .callback_count
                .is_multiple_of(DEFAULT_TIMING_SAMPLE_INTERVAL);
        let callback_started = sample_timing.then(Instant::now);
        self.shared.input_samples.fetch_add(
            u64::try_from(samples.len()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.load_parameters();

        self.callback_scratch[..samples.len()].copy_from_slice(samples);
        let dc_report = self
            .dc_blocker
            .process(&mut self.callback_scratch[..samples.len()]);
        self.shared.sanitized_samples.fetch_add(
            dc_report.non_finite.saturating_add(dc_report.subnormal),
            Ordering::Relaxed,
        );

        let Self {
            transport,
            model,
            assembler,
            dry_delay,
            wet,
            dry,
            mixed,
            strength,
            parameters,
            meter,
            meter_samples,
            pending_vad,
            shared,
            deadline_policy,
            deadline_window,
            model_count,
            failed,
            callback_scratch,
            ..
        } = self;
        let push = assembler.push(&callback_scratch[..samples.len()], |frame| {
            Self::process_frame_parts(
                transport,
                model,
                dry_delay,
                wet,
                dry,
                mixed,
                strength,
                *parameters,
                meter,
                meter_samples,
                pending_vad,
                shared,
                *deadline_policy,
                deadline_window,
                model_count,
                failed,
                frame,
            );
        });
        if push.is_err() {
            *failed = true;
            shared.model_errors.fetch_add(1, Ordering::Relaxed);
            shared
                .state
                .store(LiveState::ModelFailed as u32, Ordering::Release);
        }
        if let Some(started) = callback_started {
            shared.callback_timing.observe(started.elapsed());
        }
    }
}

/// Builds the complete fixed-capacity live path before stream activation.
///
/// # Errors
///
/// Returns [`LivePipelineError`] when the injected model contract or delay is
/// incompatible with Noire's canonical real-time path.
pub fn create_live_channel(
    model: Box<dyn Denoiser>,
) -> Result<(LiveCaptureSink, BypassOutput, LiveControl, LiveTelemetry), LivePipelineError> {
    let (transport, output, _transport_control, transport_telemetry) = create_processed_channel();
    let shared = Arc::new(SharedLiveState::default());
    let sink = LiveCaptureSink::new(transport, model, Arc::clone(&shared))?;
    Ok((
        sink,
        output,
        LiveControl {
            shared: Arc::clone(&shared),
        },
        LiveTelemetry {
            shared,
            transport: transport_telemetry,
        },
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_precision_loss, clippy::float_cmp)]

    use std::{thread, time::Duration};

    use noire_dsp::{CLICK_EXCESS_THRESHOLD, MODEL_FRAME_SAMPLES};
    use noire_model::{
        Denoiser, FrameStats, ModelDescriptor, ModelDescriptorSpec, ProcessError,
        finalize_process_output, prepare_process_frame,
    };

    use super::{DeadlinePolicy, FailMode, LiveState, create_live_channel};
    use crate::{CaptureSink, InputGeneration};

    struct TestModel {
        descriptor: ModelDescriptor,
        gain: f32,
        calls: u64,
        fail_at: Option<u64>,
        sleep: Duration,
    }

    impl TestModel {
        fn new(gain: f32) -> Result<Self, noire_model::DescriptorError> {
            Ok(Self {
                descriptor: ModelDescriptor::new(ModelDescriptorSpec {
                    id: "test.live",
                    name: "Live test model",
                    version: "1",
                    license: "MIT",
                    sample_rate_hz: 48_000,
                    channels: 1,
                    frame_samples: MODEL_FRAME_SAMPLES,
                    hop_samples: MODEL_FRAME_SAMPLES,
                    lookahead_samples: 0,
                    delay_samples: MODEL_FRAME_SAMPLES,
                })?,
                gain,
                calls: 0,
                fail_at: None,
                sleep: Duration::ZERO,
            })
        }
    }

    impl Denoiser for TestModel {
        fn descriptor(&self) -> &ModelDescriptor {
            &self.descriptor
        }

        fn reset(&mut self) {
            self.calls = 0;
        }

        fn process_frame(
            &mut self,
            input: &[f32],
            output: &mut [f32],
        ) -> Result<FrameStats, ProcessError> {
            prepare_process_frame(&self.descriptor, input, output)?;
            self.calls = self.calls.saturating_add(1);
            if self.fail_at == Some(self.calls) {
                return Err(ProcessError::ModelFailure);
            }
            if !self.sleep.is_zero() {
                thread::sleep(self.sleep);
            }
            for (source, destination) in input.iter().zip(output.iter_mut()) {
                *destination = *source * self.gain;
            }
            finalize_process_output(output, FrameStats::new(0.75)?)
        }
    }

    fn tone(frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|index| {
                let phase = 2.0 * core::f32::consts::PI * 1_000.0 * index as f32 / 48_000.0;
                phase.sin() * 0.2
            })
            .collect()
    }

    #[test]
    fn exact_frames_controls_meters_and_deactivated_reset_converge()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut sink, mut output, control, telemetry) =
            create_live_channel(Box::new(TestModel::new(0.5)?))?;
        control.set_diagnostic_timing(true);
        let input = tone(MODEL_FRAME_SAMPLES * 12);
        for chunk in input.chunks(128) {
            sink.write(InputGeneration::INITIAL, chunk);
        }
        let mut observed = vec![0.0; input.len()];
        for chunk in observed.chunks_mut(MODEL_FRAME_SAMPLES) {
            let _ = output.fill(chunk)?;
        }
        let first = telemetry.snapshot();
        assert_eq!(first.model_frames, 12);
        assert_eq!(first.model_errors, 0);
        assert_eq!(first.model_timing.samples, 12);
        assert!(first.vad_probability > 0.0);
        assert!(first.peak > 0.0);
        assert!(first.rms > 0.0);
        assert!(observed.iter().all(|sample| sample.is_finite()));

        control.set_strength(0.0);
        control.set_enabled(false);
        for frame in input[..MODEL_FRAME_SAMPLES * 4].chunks(MODEL_FRAME_SAMPLES) {
            sink.write(InputGeneration::INITIAL, frame);
            let mut discarded = [0.0; MODEL_FRAME_SAMPLES];
            let _ = output.fill(&mut discarded)?;
        }
        assert_eq!(telemetry.snapshot().model_errors, 0);

        sink.reset_deactivated();
        assert_eq!(telemetry.snapshot().model_resets, 1);
        assert_eq!(telemetry.snapshot().state, LiveState::Running);
        Ok(())
    }

    #[test]
    fn strength_and_enable_sweeps_are_click_bounded() -> Result<(), Box<dyn std::error::Error>> {
        let (mut sink, mut output, control, _telemetry) =
            create_live_channel(Box::new(TestModel::new(0.5)?))?;
        let input = tone(MODEL_FRAME_SAMPLES * 30);
        let mut observed = Vec::with_capacity(input.len());
        for (index, frame) in input.chunks(MODEL_FRAME_SAMPLES).enumerate() {
            if index == 6 {
                control.set_strength(0.0);
            } else if index == 12 {
                control.set_strength(0.65);
            } else if index == 18 {
                control.set_enabled(false);
            } else if index == 24 {
                control.set_enabled(true);
            }
            sink.write(InputGeneration::INITIAL, frame);
            let mut rendered = [0.0; MODEL_FRAME_SAMPLES];
            let _ = output.fill(&mut rendered)?;
            observed.extend_from_slice(&rendered);
        }

        let mut maximum_excess = 0.0_f32;
        for (source, rendered) in input.windows(2).zip(observed.windows(2)) {
            let source_step = (source[1] - source[0]).abs();
            let rendered_step = (rendered[1] - rendered[0]).abs();
            maximum_excess = maximum_excess.max((rendered_step - source_step).max(0.0));
        }
        assert!(
            maximum_excess <= CLICK_EXCESS_THRESHOLD,
            "transition excess was {maximum_excess}"
        );
        Ok(())
    }

    #[test]
    fn model_failure_is_closed_by_default_and_explicitly_openable()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut model = TestModel::new(-0.5)?;
        model.fail_at = Some(3);
        let (mut sink, mut output, control, telemetry) = create_live_channel(Box::new(model))?;
        let tagged = [0.8; MODEL_FRAME_SAMPLES];
        for _ in 0..4 {
            sink.write(InputGeneration::INITIAL, &tagged);
        }
        assert_eq!(telemetry.snapshot().state, LiveState::ModelFailed);
        assert_eq!(telemetry.snapshot().model_errors, 1);
        let mut rendered = [0.0; MODEL_FRAME_SAMPLES];
        let _ = output.fill(&mut rendered)?;
        let _ = output.fill(&mut rendered)?;
        let _ = output.fill(&mut rendered)?;
        assert!(rendered.iter().all(|sample| *sample <= 0.0));
        assert!(rendered.iter().all(|sample| (*sample - 0.8).abs() > 1.0e-6));

        control.set_fail_mode(FailMode::Open);
        sink.reset_deactivated();
        for _ in 0..4 {
            sink.write(InputGeneration::INITIAL, &tagged);
        }
        assert!(telemetry.snapshot().dry_frames > 0);
        assert!(telemetry.snapshot().model_resets > 0);
        Ok(())
    }

    #[test]
    fn repeated_sampled_deadline_misses_enter_degraded_performance()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut model = TestModel::new(0.5)?;
        model.sleep = Duration::from_micros(50);
        let (mut sink, _output, control, telemetry) = create_live_channel(Box::new(model))?;
        control.set_diagnostic_timing(true);
        sink.set_deadline_policy_deactivated(DeadlinePolicy {
            model_deadline: Duration::from_nanos(1),
            ..DeadlinePolicy::default()
        });
        for _ in 0..5 {
            sink.write(InputGeneration::INITIAL, &[0.1; MODEL_FRAME_SAMPLES]);
        }
        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.deadline_misses, 5);
        assert_eq!(snapshot.state, LiveState::DegradedPerformance);
        Ok(())
    }
}
