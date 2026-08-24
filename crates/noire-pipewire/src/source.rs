//! Native virtual microphone source and consumer-demand observation.

use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use libspa::{param::ParamType, pod::Pod, utils::Direction};
use noire_dsp::MAX_CALLBACK_FRAMES;
use pipewire::{
    keys,
    properties::properties,
    stream::{self, StreamFlags, StreamRc, StreamState},
};

use crate::{
    BypassOutput, NegotiatedFormatError, NegotiatedFormatEvent, PipewireConnection,
    RESERVED_NODE_NAME, build_capture_format_pod, parse_negotiated_format,
};

const BYTES_PER_SAMPLE: usize = size_of::<f32>();
const AUDIO_COMMAND_NONE: u8 = 0;
const AUDIO_COMMAND_DISCARD: u8 = 1;
const AUDIO_COMMAND_CLEAR: u8 = 2;

/// Delay before the last consumer pauses physical capture.
pub const CONSUMER_IDLE_DEBOUNCE: Duration = Duration::from_millis(500);

/// Requested `PipeWire` graph quantum for capture and publication streams.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StreamLatency {
    /// Lowest qualified quantum: 256 samples at 48 kHz.
    #[default]
    Low,
    /// More scheduling headroom: 512 samples at 48 kHz.
    Balanced,
}

impl StreamLatency {
    /// `PipeWire` `node.latency` value shared by both ends of the graph.
    #[must_use]
    pub const fn node_property(self) -> &'static str {
        match self {
            Self::Low => "256/48000",
            Self::Balanced => "512/48000",
        }
    }
}

/// Virtual-source lifecycle state copied to the control plane.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SourceStreamState {
    /// Stream proxy is not connected.
    #[default]
    Unconnected,
    /// Negotiation/connection is in progress.
    Connecting,
    /// Source exists but no consumer currently drives it.
    Paused,
    /// At least one consumer drives process callbacks.
    Streaming,
    /// `PipeWire` reported a stream error.
    Error,
}

/// Stable consumer-demand state maintained by the owning control loop.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConsumerDemand {
    /// Capture should be inactive and sensitive buffers empty.
    #[default]
    Idle,
    /// At least one source consumer requires audio.
    Active,
}

/// Edge returned exactly once when demand changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DemandTransition {
    /// First consumer began running.
    Activate,
    /// Last consumer remained absent for the full debounce.
    Deactivate,
}

#[derive(Debug, Default)]
struct ControlState {
    stream_state: SourceStreamState,
    demand: ConsumerDemand,
    idle_since: Option<Instant>,
    format_event: Option<NegotiatedFormatEvent>,
    stream_error: Option<String>,
}

#[derive(Debug, Default)]
struct SourceCounters {
    callbacks: AtomicU64,
    empty_dequeues: AtomicU64,
    missing_buffers: AtomicU64,
    malformed_buffers: AtomicU64,
}

/// Lock-free virtual-source boundary metrics.
#[derive(Clone, Debug, Default)]
pub struct SourceTelemetry {
    counters: Arc<SourceCounters>,
}

/// Immutable source-boundary metric snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SourceTelemetrySnapshot {
    /// Source process callback invocations.
    pub callbacks: u64,
    /// Callbacks for which `PipeWire` supplied no dequeuable output buffer.
    pub empty_dequeues: u64,
    /// Output buffers whose mapped storage was unavailable.
    pub missing_buffers: u64,
    /// Output buffers with invalid plane/chunk shape.
    pub malformed_buffers: u64,
}

impl SourceTelemetry {
    /// Reads source counters without locking the process callback.
    #[must_use]
    pub fn snapshot(&self) -> SourceTelemetrySnapshot {
        SourceTelemetrySnapshot {
            callbacks: self.counters.callbacks.load(Ordering::Relaxed),
            empty_dequeues: self.counters.empty_dequeues.load(Ordering::Relaxed),
            missing_buffers: self.counters.missing_buffers.load(Ordering::Relaxed),
            malformed_buffers: self.counters.malformed_buffers.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug)]
struct SourceAudio {
    output: BypassOutput,
    scratch: [f32; MAX_CALLBACK_FRAMES],
}

#[derive(Debug)]
struct SourceProcessor {
    audio: Rc<RefCell<SourceAudio>>,
    audio_command: Arc<AtomicU8>,
    telemetry: SourceTelemetry,
}

/// Construction/connect failure for the virtual source stream.
#[derive(Debug)]
pub enum SourceStreamError {
    /// The canonical MONO SPA pod could not be serialized.
    FormatPod,
    /// The native binding rejected source creation or connection.
    Native(pipewire::Error),
}

impl std::fmt::Display for SourceStreamError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FormatPod => formatter.write_str("could not serialize virtual source format pod"),
            Self::Native(error) => write!(formatter, "PipeWire virtual source error: {error}"),
        }
    }
}

impl std::error::Error for SourceStreamError {}

impl From<pipewire::Error> for SourceStreamError {
    fn from(error: pipewire::Error) -> Self {
        Self::Native(error)
    }
}

/// Stable, non-lingering `Audio/Source` backed by the processed SPSC consumer.
pub struct VirtualSourceStream {
    _listener: stream::StreamListener<SourceProcessor>,
    stream: StreamRc,
    control: Rc<RefCell<ControlState>>,
    audio: Rc<RefCell<SourceAudio>>,
    audio_command: Arc<AtomicU8>,
    telemetry: SourceTelemetry,
}

impl VirtualSourceStream {
    /// Creates and connects the canonical Noire virtual microphone.
    ///
    /// # Errors
    ///
    /// Returns a format serialization or native stream error.
    pub fn connect(
        connection: &PipewireConnection,
        output: BypassOutput,
    ) -> Result<Self, SourceStreamError> {
        Self::connect_with_latency(connection, output, StreamLatency::Low)
    }

    /// Creates and connects the canonical source with an explicit graph quantum.
    ///
    /// # Errors
    ///
    /// Returns a format serialization or native stream error.
    pub fn connect_with_latency(
        connection: &PipewireConnection,
        output: BypassOutput,
        latency: StreamLatency,
    ) -> Result<Self, SourceStreamError> {
        let properties = properties! {
            *keys::NODE_NAME => RESERVED_NODE_NAME,
            *keys::NODE_DESCRIPTION => "Noire Microphone ☾",
            *keys::NODE_NICK => "Noire",
            *keys::NODE_VIRTUAL => "true",
            *keys::NODE_PAUSE_ON_IDLE => "true",
            *keys::MEDIA_TYPE => "Audio",
            *keys::MEDIA_CATEGORY => "Capture",
            *keys::MEDIA_ROLE => "Communication",
            *keys::MEDIA_CLASS => "Audio/Source",
            "audio.rate" => "48000",
            *keys::AUDIO_CHANNELS => "1",
            "audio.position" => "[ MONO ]",
            *keys::NODE_LATENCY => latency.node_property(),
        };
        let stream = StreamRc::new(connection.core_clone(), "noire-virtual-source", properties)?;
        let control = Rc::new(RefCell::new(ControlState::default()));
        let telemetry = SourceTelemetry::default();
        let audio = Rc::new(RefCell::new(SourceAudio {
            output,
            scratch: [0.0; MAX_CALLBACK_FRAMES],
        }));
        let audio_command = Arc::new(AtomicU8::new(AUDIO_COMMAND_NONE));
        let processor = SourceProcessor {
            audio: Rc::clone(&audio),
            audio_command: Arc::clone(&audio_command),
            telemetry: telemetry.clone(),
        };

        let state_control = Rc::clone(&control);
        let format_control = Rc::clone(&control);
        let listener = stream
            .add_local_listener_with_user_data(processor)
            .state_changed(move |_stream, _processor, _old, new| {
                let mut control = state_control.borrow_mut();
                control.stream_state = map_stream_state(&new);
                match new {
                    StreamState::Streaming => control.idle_since = None,
                    StreamState::Paused if control.demand == ConsumerDemand::Active => {
                        control.idle_since.get_or_insert_with(Instant::now);
                    }
                    StreamState::Error(message) => {
                        control.stream_error = Some(message);
                        control.idle_since.get_or_insert_with(Instant::now);
                    }
                    StreamState::Unconnected => {
                        control.idle_since.get_or_insert_with(Instant::now);
                    }
                    StreamState::Connecting | StreamState::Paused => {}
                }
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

        let pod_bytes = build_capture_format_pod().map_err(|_| SourceStreamError::FormatPod)?;
        let pod = Pod::from_bytes(&pod_bytes).ok_or(SourceStreamError::FormatPod)?;
        let mut params = [pod];
        stream.connect(
            Direction::Output,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut params,
        )?;
        Ok(Self {
            _listener: listener,
            stream,
            control,
            audio,
            audio_command,
            telemetry,
        })
    }

    /// Returns the stable published node name.
    #[must_use]
    pub const fn node_name(&self) -> &'static str {
        RESERVED_NODE_NAME
    }

    /// Returns the transient graph node ID for diagnostics only.
    #[must_use]
    pub fn node_id(&self) -> u32 {
        self.stream.node_id()
    }

    /// Returns current source stream state.
    #[must_use]
    pub fn state(&self) -> SourceStreamState {
        self.control.borrow().stream_state
    }

    /// Returns the stable debounced demand state.
    #[must_use]
    pub fn demand(&self) -> ConsumerDemand {
        self.control.borrow().demand
    }

    /// Resolves immediate activation and debounced deactivation edges.
    #[must_use]
    pub fn demand_transition_if_due(&self, now: Instant) -> Option<DemandTransition> {
        resolve_demand_transition(&mut self.control.borrow_mut(), now)
    }

    /// Clears all source-owned sensitive samples without overlapping a callback borrow.
    pub fn clear_sensitive(&self) {
        self.schedule_audio_command(AUDIO_COMMAND_CLEAR);
    }

    /// Drains pending source audio without ending the current generation.
    pub fn discard_pending_sensitive(&self) {
        self.schedule_audio_command(AUDIO_COMMAND_DISCARD);
    }

    fn schedule_audio_command(&self, command: u8) {
        // CLEAR dominates DISCARD when control events race. The atomic command
        // is authoritative; applying it eagerly is only an optimization for
        // the common owner-thread path where no process callback holds audio.
        self.audio_command.fetch_max(command, Ordering::AcqRel);
        if let Ok(mut audio) = self.audio.try_borrow_mut() {
            apply_audio_command(&mut audio, &self.audio_command);
        }
    }

    /// Removes the latest negotiated format event.
    #[must_use]
    pub fn take_negotiated_format(&self) -> Option<NegotiatedFormatEvent> {
        self.control.borrow_mut().format_event.take()
    }

    /// Removes the latest native source error string.
    #[must_use]
    pub fn take_error(&self) -> Option<String> {
        self.control.borrow_mut().stream_error.take()
    }

    /// Returns a lock-free source-boundary telemetry handle.
    #[must_use]
    pub fn telemetry(&self) -> SourceTelemetry {
        self.telemetry.clone()
    }
}

fn process_available_buffers(stream: &pipewire::stream::Stream, processor: &mut SourceProcessor) {
    processor
        .telemetry
        .counters
        .callbacks
        .fetch_add(1, Ordering::Relaxed);
    let mut dequeued = false;
    while let Some(mut buffer) = stream.dequeue_buffer() {
        dequeued = true;
        let requested_frames = usize::try_from(buffer.requested()).unwrap_or(usize::MAX);
        let datas = buffer.datas_mut();
        if datas.len() != 1 {
            for data in datas {
                if let Some(bytes) = data.data() {
                    bytes.fill(0);
                }
                if !data.as_raw().chunk.is_null() {
                    let chunk = data.chunk_mut();
                    *chunk.offset_mut() = 0;
                    *chunk.size_mut() = 0;
                    *chunk.stride_mut() = i32::try_from(BYTES_PER_SAMPLE).unwrap_or(i32::MAX);
                }
            }
            processor
                .telemetry
                .counters
                .malformed_buffers
                .fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let data = &mut datas[0];
        if data.as_raw().chunk.is_null() {
            processor
                .telemetry
                .counters
                .malformed_buffers
                .fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let Some(bytes) = data.data() else {
            processor
                .telemetry
                .counters
                .missing_buffers
                .fetch_add(1, Ordering::Relaxed);
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.size_mut() = 0;
            *chunk.stride_mut() = i32::try_from(BYTES_PER_SAMPLE).unwrap_or(i32::MAX);
            continue;
        };
        let mapped_frames = bytes.len() / BYTES_PER_SAMPLE;
        let frame_count = if requested_frames == 0 {
            mapped_frames
        } else {
            requested_frames
        };
        if frame_count > MAX_CALLBACK_FRAMES
            || frame_count > mapped_frames
            || !bytes.len().is_multiple_of(BYTES_PER_SAMPLE)
        {
            bytes.fill(0);
            processor
                .audio
                .borrow_mut()
                .output
                .reject_oversized_request(frame_count);
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.size_mut() = 0;
            *chunk.stride_mut() = i32::try_from(BYTES_PER_SAMPLE).unwrap_or(i32::MAX);
            continue;
        }

        let mut audio = processor.audio.borrow_mut();
        apply_audio_command(&mut audio, &processor.audio_command);
        let SourceAudio { output, scratch } = &mut *audio;
        let samples = &mut scratch[..frame_count];
        let published_frames = fill_output(output, samples);
        for (sample, destination) in samples[..published_frames]
            .iter()
            .zip(bytes[..published_frames * BYTES_PER_SAMPLE].chunks_exact_mut(BYTES_PER_SAMPLE))
        {
            destination.copy_from_slice(&sample.to_ne_bytes());
        }
        let byte_count = published_frames.saturating_mul(BYTES_PER_SAMPLE);
        let chunk = data.chunk_mut();
        *chunk.offset_mut() = 0;
        *chunk.size_mut() = u32::try_from(byte_count).unwrap_or(u32::MAX);
        *chunk.stride_mut() = i32::try_from(BYTES_PER_SAMPLE).unwrap_or(i32::MAX);
    }
    if !dequeued {
        processor
            .telemetry
            .counters
            .empty_dequeues
            .fetch_add(1, Ordering::Relaxed);
    }
}

fn apply_audio_command(audio: &mut SourceAudio, command: &AtomicU8) {
    match command.swap(AUDIO_COMMAND_NONE, Ordering::AcqRel) {
        AUDIO_COMMAND_CLEAR => audio.output.clear_sensitive(),
        AUDIO_COMMAND_DISCARD => audio.output.discard_pending_sensitive(),
        _ => return,
    }
    audio.scratch.fill(0.0);
}

fn fill_output(output: &mut BypassOutput, samples: &mut [f32]) -> usize {
    if let Ok(report) = output.fill(samples) {
        usize::from(report.frames)
    } else {
        samples.fill(0.0);
        0
    }
}

fn map_stream_state(state: &StreamState) -> SourceStreamState {
    match state {
        StreamState::Unconnected => SourceStreamState::Unconnected,
        StreamState::Connecting => SourceStreamState::Connecting,
        StreamState::Paused => SourceStreamState::Paused,
        StreamState::Streaming => SourceStreamState::Streaming,
        StreamState::Error(_) => SourceStreamState::Error,
    }
}

fn resolve_demand_transition(control: &mut ControlState, now: Instant) -> Option<DemandTransition> {
    if control.stream_state == SourceStreamState::Streaming
        && control.demand == ConsumerDemand::Idle
    {
        control.demand = ConsumerDemand::Active;
        control.idle_since = None;
        return Some(DemandTransition::Activate);
    }
    if control.demand == ConsumerDemand::Active
        && control.stream_state != SourceStreamState::Streaming
        && control
            .idle_since
            .is_some_and(|since| now.saturating_duration_since(since) >= CONSUMER_IDLE_DEBOUNCE)
    {
        control.demand = ConsumerDemand::Idle;
        control.idle_since = None;
        return Some(DemandTransition::Deactivate);
    }
    None
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicU8, Ordering},
        time::{Duration, Instant},
    };

    use super::{
        AUDIO_COMMAND_CLEAR, AUDIO_COMMAND_DISCARD, AUDIO_COMMAND_NONE, CONSUMER_IDLE_DEBOUNCE,
        ConsumerDemand, ControlState, DemandTransition, SourceAudio, SourceStreamState,
        StreamLatency, apply_audio_command, resolve_demand_transition,
    };
    use crate::create_bypass_channel;
    use noire_dsp::MAX_CALLBACK_FRAMES;

    #[test]
    fn latency_profiles_request_distinct_bounded_quanta() {
        assert_eq!(StreamLatency::Low.node_property(), "256/48000");
        assert_eq!(StreamLatency::Balanced.node_property(), "512/48000");
    }

    #[test]
    fn first_consumer_is_immediate_and_last_consumer_is_debounced() {
        let started = Instant::now();
        let mut state = ControlState {
            stream_state: SourceStreamState::Streaming,
            ..ControlState::default()
        };
        assert_eq!(
            resolve_demand_transition(&mut state, started),
            Some(DemandTransition::Activate)
        );
        assert_eq!(state.demand, ConsumerDemand::Active);
        state.stream_state = SourceStreamState::Paused;
        state.idle_since = Some(started);
        assert_eq!(
            resolve_demand_transition(
                &mut state,
                started + CONSUMER_IDLE_DEBOUNCE.saturating_sub(Duration::from_millis(1))
            ),
            None
        );
        assert_eq!(
            resolve_demand_transition(&mut state, started + CONSUMER_IDLE_DEBOUNCE),
            Some(DemandTransition::Deactivate)
        );
        assert_eq!(state.demand, ConsumerDemand::Idle);
    }

    #[test]
    fn callback_side_clear_dominates_a_pending_discard() {
        let (_producer, output, _control, _telemetry) = create_bypass_channel();
        let mut audio = SourceAudio {
            output,
            scratch: [1.0; MAX_CALLBACK_FRAMES],
        };
        let command = AtomicU8::new(AUDIO_COMMAND_DISCARD);
        command.fetch_max(AUDIO_COMMAND_CLEAR, Ordering::AcqRel);

        apply_audio_command(&mut audio, &command);

        assert_eq!(command.load(Ordering::Acquire), AUDIO_COMMAND_NONE);
        assert!(audio.scratch.iter().all(|sample| *sample == 0.0));
    }
}
