//! Bounded capture-buffer validation, decoding, metering, and native stream glue.

use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
};

use noire_dsp::{MAX_CALLBACK_FRAMES, Meter, MeterSnapshot, SanitizeReport, sanitize_buffer};

const F32_BYTES: usize = size_of::<f32>();
const F32_BYTES_U32: u32 = 4;

/// Monotonic identity for samples produced by one selected input lifecycle.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct InputGeneration(u64);

impl InputGeneration {
    /// First generation assigned when capture state is constructed.
    pub const INITIAL: Self = Self(1);

    /// Returns the next generation, saturating only at the numeric limit.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// Returns the stable numeric value used by atomic telemetry.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl Default for InputGeneration {
    fn default() -> Self {
        Self::INITIAL
    }
}

/// Trusted copy of one SPA chunk's metadata.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ChunkMetadata {
    /// Byte offset from the mapped data-plane start.
    pub offset_bytes: u32,
    /// Declared byte length.
    pub size_bytes: u32,
    /// Byte stride, where zero means tightly packed.
    pub stride_bytes: i32,
    /// Whether SPA marked this chunk corrupted.
    pub corrupted: bool,
}

/// Error rejecting untrusted capture-buffer metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureBufferError {
    /// Metadata does not describe an aligned accessible `f32` region.
    MalformedChunk,
    /// The declared quantum exceeds the fixed callback bound.
    QuantumTooLarge,
}

impl fmt::Display for CaptureBufferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MalformedChunk => "capture chunk metadata is malformed",
            Self::QuantumTooLarge => "capture chunk exceeds the fixed callback bound",
        })
    }
}

impl std::error::Error for CaptureBufferError {}

/// Callback counters retained by the processor and mirrored atomically.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CaptureCounters {
    /// Process callback invocations.
    pub callbacks: u64,
    /// Successfully delivered canonical frames.
    pub frames: u64,
    /// Empty mapped buffers/chunks.
    pub empty_buffers: u64,
    /// Rejected malformed chunks.
    pub malformed_chunks: u64,
    /// Rejected oversized chunks.
    pub oversized_chunks: u64,
    /// Non-finite samples replaced with silence.
    pub non_finite_samples: u64,
    /// Subnormal samples flushed to silence.
    pub subnormal_samples: u64,
    /// Input changes that cleared all generation-local state.
    pub input_generation_resets: u64,
}

/// One successful bounded capture call.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CaptureReport {
    /// Input lifecycle that owns the delivered frames.
    pub generation: InputGeneration,
    /// Canonical frames delivered to the sink.
    pub frames: u16,
    /// Invalid values sanitized at the boundary.
    pub sanitized: SanitizeReport,
}

/// Allocation-free destination called with validated canonical samples.
pub trait CaptureSink {
    /// Clears queued/stateful data before samples from a new input arrive.
    fn reset(&mut self, _generation: InputGeneration) {}

    /// Accepts one finite mono callback slice.
    fn write(&mut self, generation: InputGeneration, samples: &[f32]);
}

/// Lock-free callback telemetry readable by the control plane.
#[derive(Clone, Debug, Default)]
pub struct CaptureTelemetry {
    inner: Arc<CaptureTelemetryInner>,
}

#[derive(Debug)]
struct CaptureTelemetryInner {
    generation: AtomicU64,
    callbacks: AtomicU64,
    frames: AtomicU64,
    empty_buffers: AtomicU64,
    malformed_chunks: AtomicU64,
    oversized_chunks: AtomicU64,
    non_finite_samples: AtomicU64,
    subnormal_samples: AtomicU64,
    input_generation_resets: AtomicU64,
    peak_bits: AtomicU32,
}

impl Default for CaptureTelemetryInner {
    fn default() -> Self {
        Self {
            generation: AtomicU64::new(InputGeneration::INITIAL.get()),
            callbacks: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            empty_buffers: AtomicU64::new(0),
            malformed_chunks: AtomicU64::new(0),
            oversized_chunks: AtomicU64::new(0),
            non_finite_samples: AtomicU64::new(0),
            subnormal_samples: AtomicU64::new(0),
            input_generation_resets: AtomicU64::new(0),
            peak_bits: AtomicU32::new(0),
        }
    }
}

/// Immutable control-plane snapshot of capture telemetry.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CaptureTelemetrySnapshot {
    /// Input lifecycle represented by meter and callback state.
    pub generation: InputGeneration,
    /// Current callback counters.
    pub counters: CaptureCounters,
    /// Largest absolute finite sample observed since construction.
    pub peak: f32,
}

impl CaptureTelemetry {
    /// Reads all counters without locking the process callback.
    #[must_use]
    pub fn snapshot(&self) -> CaptureTelemetrySnapshot {
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        CaptureTelemetrySnapshot {
            generation: InputGeneration(load(&self.inner.generation)),
            counters: CaptureCounters {
                callbacks: load(&self.inner.callbacks),
                frames: load(&self.inner.frames),
                empty_buffers: load(&self.inner.empty_buffers),
                malformed_chunks: load(&self.inner.malformed_chunks),
                oversized_chunks: load(&self.inner.oversized_chunks),
                non_finite_samples: load(&self.inner.non_finite_samples),
                subnormal_samples: load(&self.inner.subnormal_samples),
                input_generation_resets: load(&self.inner.input_generation_resets),
            },
            peak: f32::from_bits(self.inner.peak_bits.load(Ordering::Relaxed)),
        }
    }

    fn publish(&self, counters: CaptureCounters, generation: InputGeneration, samples: &[f32]) {
        self.inner
            .generation
            .store(generation.get(), Ordering::Relaxed);
        self.inner
            .callbacks
            .store(counters.callbacks, Ordering::Relaxed);
        self.inner.frames.store(counters.frames, Ordering::Relaxed);
        self.inner
            .empty_buffers
            .store(counters.empty_buffers, Ordering::Relaxed);
        self.inner
            .malformed_chunks
            .store(counters.malformed_chunks, Ordering::Relaxed);
        self.inner
            .oversized_chunks
            .store(counters.oversized_chunks, Ordering::Relaxed);
        self.inner
            .non_finite_samples
            .store(counters.non_finite_samples, Ordering::Relaxed);
        self.inner
            .subnormal_samples
            .store(counters.subnormal_samples, Ordering::Relaxed);
        self.inner
            .input_generation_resets
            .store(counters.input_generation_resets, Ordering::Relaxed);
        let peak = samples
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        self.inner
            .peak_bits
            .fetch_max(peak.to_bits(), Ordering::Relaxed);
    }

    fn reset_generation(&self, counters: CaptureCounters, generation: InputGeneration) {
        self.inner.peak_bits.store(0, Ordering::Relaxed);
        self.publish(counters, generation, &[]);
    }
}

/// Fixed-storage capture processor used directly by the native callback.
#[derive(Debug)]
pub struct CaptureProcessor<S> {
    sink: S,
    scratch: [f32; MAX_CALLBACK_FRAMES],
    meter: Meter,
    counters: CaptureCounters,
    telemetry: CaptureTelemetry,
    generation: InputGeneration,
    generation_command: Arc<AtomicU64>,
}

impl<S: CaptureSink> CaptureProcessor<S> {
    /// Constructs all callback storage before stream activation.
    #[must_use]
    pub fn new(sink: S, telemetry: CaptureTelemetry) -> Self {
        Self::with_generation_command(
            sink,
            telemetry,
            Arc::new(AtomicU64::new(InputGeneration::INITIAL.get())),
        )
    }

    fn with_generation_command(
        sink: S,
        telemetry: CaptureTelemetry,
        generation_command: Arc<AtomicU64>,
    ) -> Self {
        Self {
            sink,
            scratch: [0.0; MAX_CALLBACK_FRAMES],
            meter: Meter::new(),
            counters: CaptureCounters::default(),
            telemetry,
            generation: InputGeneration::INITIAL,
            generation_command,
        }
    }

    /// Validates, decodes, sanitizes, meters, and delivers one mapped chunk.
    ///
    /// # Errors
    ///
    /// Returns a compact error for malformed or oversized metadata. The sink is
    /// not called on errors or empty chunks.
    pub fn process_mapped(
        &mut self,
        mapped: Option<&[u8]>,
        metadata: ChunkMetadata,
    ) -> Result<CaptureReport, CaptureBufferError> {
        self.synchronize_generation();
        self.counters.callbacks = self.counters.callbacks.saturating_add(1);
        if metadata.size_bytes == 0 {
            self.counters.empty_buffers = self.counters.empty_buffers.saturating_add(1);
            self.telemetry.publish(self.counters, self.generation, &[]);
            return Ok(CaptureReport {
                generation: self.generation,
                ..CaptureReport::default()
            });
        }
        if metadata.corrupted
            || !matches!(metadata.stride_bytes, 0 | 4)
            || !metadata.size_bytes.is_multiple_of(F32_BYTES_U32)
        {
            return self.reject(CaptureBufferError::MalformedChunk);
        }
        let Some(mapped) = mapped else {
            return self.reject(CaptureBufferError::MalformedChunk);
        };
        let Ok(offset) = usize::try_from(metadata.offset_bytes) else {
            return self.reject(CaptureBufferError::MalformedChunk);
        };
        let Ok(size) = usize::try_from(metadata.size_bytes) else {
            return self.reject(CaptureBufferError::MalformedChunk);
        };
        let Some(end) = offset.checked_add(size) else {
            return self.reject(CaptureBufferError::MalformedChunk);
        };
        let Some(bytes) = mapped.get(offset..end) else {
            return self.reject(CaptureBufferError::MalformedChunk);
        };
        let frames = bytes.len() / F32_BYTES;
        if frames > MAX_CALLBACK_FRAMES {
            return self.reject(CaptureBufferError::QuantumTooLarge);
        }

        for (bytes, sample) in bytes
            .chunks_exact(F32_BYTES)
            .zip(self.scratch[..frames].iter_mut())
        {
            let array = <[u8; F32_BYTES]>::try_from(bytes)
                .map_err(|_| CaptureBufferError::MalformedChunk)?;
            *sample = f32::from_ne_bytes(array);
        }
        let samples = &mut self.scratch[..frames];
        let sanitized = sanitize_buffer(samples);
        self.meter.observe(samples);
        self.sink.write(self.generation, samples);
        self.counters.frames = self
            .counters
            .frames
            .saturating_add(u64::try_from(frames).unwrap_or(u64::MAX));
        self.counters.non_finite_samples = self
            .counters
            .non_finite_samples
            .saturating_add(sanitized.non_finite);
        self.counters.subnormal_samples = self
            .counters
            .subnormal_samples
            .saturating_add(sanitized.subnormal);
        self.telemetry
            .publish(self.counters, self.generation, samples);
        Ok(CaptureReport {
            generation: self.generation,
            frames: u16::try_from(frames).unwrap_or(u16::MAX),
            sanitized,
        })
    }

    /// Returns processor-local counters for deterministic tests.
    #[must_use]
    pub const fn counters(&self) -> CaptureCounters {
        self.counters
    }

    /// Clears state before accepting samples from `generation`.
    pub fn reset_input_generation(&mut self, generation: InputGeneration) {
        self.generation_command
            .store(generation.get(), Ordering::Release);
        self.apply_generation(generation);
    }

    /// Returns the input lifecycle currently accepted by this processor.
    #[must_use]
    pub const fn input_generation(&self) -> InputGeneration {
        self.generation
    }

    /// Returns the current bounded meter window.
    #[must_use]
    pub fn meter_snapshot(&self) -> MeterSnapshot {
        self.meter.snapshot()
    }

    /// Returns the sink for control-plane inspection after deactivation.
    #[must_use]
    pub const fn sink(&self) -> &S {
        &self.sink
    }

    fn reject<T>(&mut self, error: CaptureBufferError) -> Result<T, CaptureBufferError> {
        match error {
            CaptureBufferError::MalformedChunk => {
                self.counters.malformed_chunks = self.counters.malformed_chunks.saturating_add(1);
            }
            CaptureBufferError::QuantumTooLarge => {
                self.counters.oversized_chunks = self.counters.oversized_chunks.saturating_add(1);
            }
        }
        self.telemetry.publish(self.counters, self.generation, &[]);
        Err(error)
    }

    fn synchronize_generation(&mut self) {
        let requested = InputGeneration(self.generation_command.load(Ordering::Acquire));
        self.apply_generation(requested);
    }

    fn apply_generation(&mut self, generation: InputGeneration) {
        if generation == self.generation {
            return;
        }
        self.scratch.fill(0.0);
        self.meter = Meter::new();
        self.sink.reset(generation);
        self.generation = generation;
        self.counters.input_generation_resets =
            self.counters.input_generation_resets.saturating_add(1);
        self.telemetry
            .reset_generation(self.counters, self.generation);
    }
}

#[cfg(feature = "pipewire-backend")]
mod native {
    use std::{
        cell::RefCell,
        rc::Rc,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };

    use libspa::{param::ParamType, pod::Pod, utils::Direction};
    use pipewire::{
        keys,
        properties::properties,
        stream::{self, StreamFlags, StreamRc, StreamState},
    };

    use super::{CaptureProcessor, CaptureSink, CaptureTelemetry, ChunkMetadata, InputGeneration};
    use crate::{
        CaptureFormat, NegotiatedFormatError, PipewireConnection, StreamLatency,
        build_capture_format_pod, parse_negotiated_format,
    };

    #[derive(Debug)]
    struct DiscardSink;

    impl CaptureSink for DiscardSink {
        fn write(&mut self, _generation: InputGeneration, _samples: &[f32]) {}
    }

    struct ErasedSink(Box<dyn CaptureSink>);

    impl std::fmt::Debug for ErasedSink {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("ErasedSink(..)")
        }
    }

    impl CaptureSink for ErasedSink {
        fn reset(&mut self, generation: InputGeneration) {
            self.0.reset(generation);
        }

        fn write(&mut self, generation: InputGeneration, samples: &[f32]) {
            self.0.write(generation, samples);
        }
    }

    type SharedProcessor = Rc<RefCell<CaptureProcessor<ErasedSink>>>;

    /// Stream lifecycle state copied for the control plane.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub enum CaptureStreamState {
        /// Stream proxy is not connected.
        #[default]
        Unconnected,
        /// Negotiation/connection is in progress.
        Connecting,
        /// Stream is connected but paused.
        Paused,
        /// Process callbacks are active.
        Streaming,
        /// `PipeWire` reported a stream error.
        Error,
    }

    /// Negotiated format event retrieved and logged by the control plane.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum NegotiatedFormatEvent {
        /// Canonical format was accepted.
        Accepted(CaptureFormat),
        /// Negotiation produced an unsupported or malformed format.
        Rejected(NegotiatedFormatError),
    }

    #[derive(Debug, Default)]
    struct ControlState {
        stream_state: CaptureStreamState,
        format_event: Option<NegotiatedFormatEvent>,
    }

    /// Construction/connect failure for the native capture stream.
    #[derive(Debug)]
    pub enum CaptureStreamError {
        /// The fixed canonical SPA pod could not be serialized.
        FormatPod,
        /// The native `PipeWire` binding rejected stream creation/connection.
        Native(pipewire::Error),
    }

    impl std::fmt::Display for CaptureStreamError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::FormatPod => formatter.write_str("could not serialize capture format pod"),
                Self::Native(error) => write!(formatter, "PipeWire capture stream error: {error}"),
            }
        }
    }

    impl std::error::Error for CaptureStreamError {}

    impl From<pipewire::Error> for CaptureStreamError {
        fn from(error: pipewire::Error) -> Self {
            Self::Native(error)
        }
    }

    /// Connected native capture stream with allocation-free process user data.
    pub struct NativeCaptureStream {
        _listener: stream::StreamListener<SharedProcessor>,
        stream: StreamRc,
        control: Rc<RefCell<ControlState>>,
        telemetry: CaptureTelemetry,
        generation_command: Arc<AtomicU64>,
        processor: SharedProcessor,
    }

    impl NativeCaptureStream {
        /// Creates and connects an input stream targeting a stable node name.
        ///
        /// # Errors
        ///
        /// Returns a format serialization or native stream error.
        pub fn connect(
            connection: &PipewireConnection,
            target_node_name: &str,
        ) -> Result<Self, CaptureStreamError> {
            Self::connect_with_sink(connection, target_node_name, DiscardSink, true)
        }

        /// Creates a capture stream with a caller-owned allocation-free sink.
        ///
        /// `initially_active = false` publishes the stream without consuming the
        /// selected microphone until demand activates it.
        ///
        /// # Errors
        ///
        /// Returns a format serialization or native stream error.
        pub fn connect_with_sink<S: CaptureSink + 'static>(
            connection: &PipewireConnection,
            target_node_name: &str,
            sink: S,
            initially_active: bool,
        ) -> Result<Self, CaptureStreamError> {
            Self::connect_with_sink_at(
                connection,
                target_node_name,
                None,
                sink,
                initially_active,
                StreamLatency::Low,
            )
        }

        pub(crate) fn connect_with_sink_to_id<S: CaptureSink + 'static>(
            connection: &PipewireConnection,
            target_node_name: &str,
            target_node_id: u32,
            sink: S,
            initially_active: bool,
        ) -> Result<Self, CaptureStreamError> {
            Self::connect_with_sink_to_id_and_latency(
                connection,
                target_node_name,
                target_node_id,
                sink,
                initially_active,
                StreamLatency::Low,
            )
        }

        pub(crate) fn connect_with_sink_to_id_and_latency<S: CaptureSink + 'static>(
            connection: &PipewireConnection,
            target_node_name: &str,
            target_node_id: u32,
            sink: S,
            initially_active: bool,
            latency: StreamLatency,
        ) -> Result<Self, CaptureStreamError> {
            Self::connect_with_sink_at(
                connection,
                target_node_name,
                Some(target_node_id),
                sink,
                initially_active,
                latency,
            )
        }

        fn connect_with_sink_at<S: CaptureSink + 'static>(
            connection: &PipewireConnection,
            target_node_name: &str,
            target_node_id: Option<u32>,
            sink: S,
            initially_active: bool,
            latency: StreamLatency,
        ) -> Result<Self, CaptureStreamError> {
            let properties = properties! {
                *keys::MEDIA_TYPE => "Audio",
                *keys::MEDIA_CATEGORY => "Capture",
                *keys::MEDIA_ROLE => "Communication",
                "target.object" => target_node_name,
                *keys::NODE_LATENCY => latency.node_property(),
            };
            let stream = StreamRc::new(connection.core_clone(), "noire-capture", properties)?;
            let control = Rc::new(RefCell::new(ControlState::default()));
            let telemetry = CaptureTelemetry::default();
            let generation_command = Arc::new(AtomicU64::new(InputGeneration::INITIAL.get()));
            let processor = Rc::new(RefCell::new(CaptureProcessor::with_generation_command(
                ErasedSink(Box::new(sink)),
                telemetry.clone(),
                Arc::clone(&generation_command),
            )));
            let state_control = Rc::clone(&control);
            let format_control = Rc::clone(&control);
            let listener = stream
                .add_local_listener_with_user_data(Rc::clone(&processor))
                .state_changed(move |_stream, _processor, _old, new| {
                    state_control.borrow_mut().stream_state = map_stream_state(&new);
                })
                .param_changed(move |_stream, _processor, id, param| {
                    if id != ParamType::Format.as_raw() {
                        return;
                    }
                    let event = param.map_or(
                        NegotiatedFormatEvent::Rejected(NegotiatedFormatError::Malformed),
                        |param| match parse_negotiated_format(param) {
                            Ok(format) => NegotiatedFormatEvent::Accepted(format),
                            Err(error) => NegotiatedFormatEvent::Rejected(error),
                        },
                    );
                    format_control.borrow_mut().format_event = Some(event);
                })
                .process(process_available_buffers)
                .register()?;

            let pod_bytes =
                build_capture_format_pod().map_err(|_| CaptureStreamError::FormatPod)?;
            let pod = Pod::from_bytes(&pod_bytes).ok_or(CaptureStreamError::FormatPod)?;
            let mut params = [pod];
            let mut flags =
                StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS;
            if target_node_id.is_some() {
                flags |= StreamFlags::DONT_RECONNECT;
            }
            if !initially_active {
                flags |= StreamFlags::INACTIVE;
            }
            stream.connect(Direction::Input, target_node_id, flags, &mut params)?;
            Ok(Self {
                _listener: listener,
                stream,
                control,
                telemetry,
                generation_command,
                processor,
            })
        }

        /// Returns current stream lifecycle state.
        #[must_use]
        pub fn state(&self) -> CaptureStreamState {
            self.control.borrow().stream_state
        }

        /// Removes the latest format event for control-plane logging/policy.
        #[must_use]
        pub fn take_negotiated_format(&self) -> Option<NegotiatedFormatEvent> {
            self.control.borrow_mut().format_event.take()
        }

        /// Returns a lock-free capture telemetry handle.
        #[must_use]
        pub fn telemetry(&self) -> CaptureTelemetry {
            self.telemetry.clone()
        }

        /// Advances the input lifecycle and schedules callback-state reset.
        #[must_use]
        pub fn advance_input_generation(&self) -> InputGeneration {
            let mut current = self.generation_command.load(Ordering::Acquire);
            loop {
                let next = current.saturating_add(1);
                match self.generation_command.compare_exchange_weak(
                    current,
                    next,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        let generation = InputGeneration(next);
                        // PipeWire may deliver a demand edge re-entrantly from the
                        // source callback while the capture processor is still
                        // handling a buffer. The atomic command is authoritative;
                        // apply it eagerly only when the callback does not already
                        // hold the processor borrow. Otherwise the next buffer
                        // synchronizes the generation before accepting samples.
                        if let Ok(mut processor) = self.processor.try_borrow_mut() {
                            processor.reset_input_generation(generation);
                        }
                        return generation;
                    }
                    Err(actual) => current = actual,
                }
            }
        }

        /// Activates or pauses capture without destroying callback state.
        ///
        /// # Errors
        ///
        /// Returns the native stream error if the state change is rejected.
        pub fn set_active(&self, active: bool) -> Result<(), pipewire::Error> {
            self.stream.set_active(active)
        }
    }

    impl Drop for NativeCaptureStream {
        fn drop(&mut self) {
            // Disconnect synchronously while the owning PipeWire core and
            // listener are still alive. Relying only on the final proxy drop
            // can leave a consumer link visible until a later main-loop turn.
            let _ = self.stream.disconnect();
        }
    }

    fn process_available_buffers(
        stream: &pipewire::stream::Stream,
        processor: &mut SharedProcessor,
    ) {
        let mut processor = processor.borrow_mut();
        while let Some(mut buffer) = stream.dequeue_buffer() {
            let datas = buffer.datas_mut();
            if datas.len() != 1 {
                let _ = processor.process_mapped(
                    None,
                    ChunkMetadata {
                        size_bytes: 1,
                        ..ChunkMetadata::default()
                    },
                );
                continue;
            }
            let data = &mut datas[0];
            if data.as_raw().chunk.is_null() {
                let _ = processor.process_mapped(
                    None,
                    ChunkMetadata {
                        size_bytes: 1,
                        ..ChunkMetadata::default()
                    },
                );
                continue;
            }
            let chunk = data.chunk();
            let metadata = ChunkMetadata {
                offset_bytes: chunk.offset(),
                size_bytes: chunk.size(),
                stride_bytes: chunk.stride(),
                corrupted: !chunk.flags().is_empty(),
            };
            let mapped = data.data().map(|bytes| &*bytes);
            let _ = processor.process_mapped(mapped, metadata);
        }
    }

    fn map_stream_state(state: &StreamState) -> CaptureStreamState {
        match state {
            StreamState::Unconnected => CaptureStreamState::Unconnected,
            StreamState::Connecting => CaptureStreamState::Connecting,
            StreamState::Paused => CaptureStreamState::Paused,
            StreamState::Streaming => CaptureStreamState::Streaming,
            StreamState::Error(_) => CaptureStreamState::Error,
        }
    }
}

#[cfg(feature = "pipewire-backend")]
pub use native::{
    CaptureStreamError, CaptureStreamState, NativeCaptureStream, NegotiatedFormatEvent,
};

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::{
        CaptureBufferError, CaptureProcessor, CaptureSink, CaptureTelemetry, ChunkMetadata,
        InputGeneration,
    };
    use noire_dsp::MAX_CALLBACK_FRAMES;

    #[derive(Debug)]
    struct FixedSink {
        samples: [f32; 8],
        length: usize,
        resets: u64,
        generation: InputGeneration,
    }

    impl FixedSink {
        const fn new() -> Self {
            Self {
                samples: [0.0; 8],
                length: 0,
                resets: 0,
                generation: InputGeneration::INITIAL,
            }
        }
    }

    impl CaptureSink for FixedSink {
        fn reset(&mut self, generation: InputGeneration) {
            self.samples.fill(0.0);
            self.length = 0;
            self.resets = self.resets.saturating_add(1);
            self.generation = generation;
        }

        fn write(&mut self, generation: InputGeneration, samples: &[f32]) {
            assert_eq!(generation, self.generation);
            let remaining = self.samples.len().saturating_sub(self.length);
            let count = samples.len().min(remaining);
            let end = self.length + count;
            self.samples[self.length..end].copy_from_slice(&samples[..count]);
            self.length = end;
        }
    }

    fn encoded(samples: &[f32]) -> Vec<u8> {
        samples
            .iter()
            .flat_map(|sample| sample.to_ne_bytes())
            .collect()
    }

    #[test]
    fn validates_offset_decodes_sanitizes_and_meters() -> Result<(), CaptureBufferError> {
        let mut bytes = vec![9, 9, 9, 9];
        bytes.extend(encoded(&[0.25, f32::NAN, -0.5]));
        bytes.extend([8, 8, 8, 8]);
        let telemetry = CaptureTelemetry::default();
        let mut processor = CaptureProcessor::new(FixedSink::new(), telemetry.clone());
        let report = processor.process_mapped(
            Some(&bytes),
            ChunkMetadata {
                offset_bytes: 4,
                size_bytes: 12,
                stride_bytes: 4,
                corrupted: false,
            },
        )?;

        assert_eq!(report.frames, 3);
        assert_eq!(report.generation, InputGeneration::INITIAL);
        assert_eq!(report.sanitized.non_finite, 1);
        assert_eq!(processor.sink().samples[..3], [0.25, 0.0, -0.5]);
        assert_eq!(processor.meter_snapshot().peak, 0.5);
        assert_eq!(telemetry.snapshot().counters.frames, 3);
        assert_eq!(telemetry.snapshot().peak, 0.5);
        Ok(())
    }

    #[test]
    fn empty_and_malformed_buffers_are_bounded_and_do_not_reach_sink() {
        let telemetry = CaptureTelemetry::default();
        let mut processor = CaptureProcessor::new(FixedSink::new(), telemetry);
        assert_eq!(
            processor.process_mapped(None, ChunkMetadata::default()),
            Ok(super::CaptureReport::default())
        );
        assert_eq!(
            processor.process_mapped(
                Some(&[0; 4]),
                ChunkMetadata {
                    offset_bytes: 3,
                    size_bytes: 4,
                    stride_bytes: 4,
                    corrupted: false,
                },
            ),
            Err(CaptureBufferError::MalformedChunk)
        );
        assert_eq!(processor.sink().length, 0);
        assert_eq!(processor.counters().empty_buffers, 1);
        assert_eq!(processor.counters().malformed_chunks, 1);
    }

    #[test]
    fn oversized_quantum_is_rejected_before_decoding() {
        let mut processor = CaptureProcessor::new(FixedSink::new(), CaptureTelemetry::default());
        let bytes = vec![0; (MAX_CALLBACK_FRAMES + 1) * size_of::<f32>()];
        assert_eq!(
            processor.process_mapped(
                Some(&bytes),
                ChunkMetadata {
                    offset_bytes: 0,
                    size_bytes: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
                    stride_bytes: 4,
                    corrupted: false,
                },
            ),
            Err(CaptureBufferError::QuantumTooLarge)
        );
        assert_eq!(processor.counters().oversized_chunks, 1);
    }

    #[test]
    fn generation_reset_clears_all_input_local_state_before_new_samples() {
        let telemetry = CaptureTelemetry::default();
        let mut processor = CaptureProcessor::new(FixedSink::new(), telemetry.clone());
        let first = encoded(&[0.75]);
        assert!(
            processor
                .process_mapped(
                    Some(&first),
                    ChunkMetadata {
                        size_bytes: 4,
                        stride_bytes: 4,
                        ..ChunkMetadata::default()
                    },
                )
                .is_ok()
        );

        let second_generation = InputGeneration::INITIAL.next();
        processor.reset_input_generation(second_generation);
        assert_eq!(processor.input_generation(), second_generation);
        assert_eq!(processor.sink().length, 0);
        assert_eq!(processor.sink().resets, 1);
        assert_eq!(processor.meter_snapshot().samples, 0);
        assert_eq!(telemetry.snapshot().generation, second_generation);
        assert_eq!(telemetry.snapshot().peak, 0.0);

        let second = encoded(&[0.25]);
        let report = processor
            .process_mapped(
                Some(&second),
                ChunkMetadata {
                    size_bytes: 4,
                    stride_bytes: 4,
                    ..ChunkMetadata::default()
                },
            )
            .ok();
        assert_eq!(
            report.map(|report| report.generation),
            Some(second_generation)
        );
        assert_eq!(processor.sink().samples[0], 0.25);
        assert_eq!(telemetry.snapshot().peak, 0.25);
        assert_eq!(telemetry.snapshot().counters.input_generation_resets, 1);
    }
}
