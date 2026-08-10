//! Native deterministic source used only by disposable `PipeWire` test sessions.

use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use libspa::{pod::Pod, utils::Direction};
use pipewire::{
    keys,
    properties::properties,
    stream::{self, StreamFlags, StreamRc},
};

use crate::{PipewireConnection, format::build_raw_audio_format_pod};

/// Fixed hardware-facing rate used to prove graph conversion to 48 kHz.
pub const SYNTHETIC_SOURCE_RATE: u32 = 44_100;

const BYTES_PER_SAMPLE: usize = size_of::<f32>();
const TONE_HERTZ: f32 = 997.0;
const TONE_AMPLITUDE: f32 = 0.18;
const NOISE_AMPLITUDE: f32 = 0.015;
const SYNTHETIC_SOURCE_RATE_F32: f32 = 44_100.0;

/// Construction/connect failure for a synthetic native source.
#[derive(Debug)]
pub enum SyntheticSourceError {
    /// The fixed source SPA pod could not be serialized.
    FormatPod,
    /// The native binding rejected source creation or connection.
    Native(pipewire::Error),
}

impl std::fmt::Display for SyntheticSourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FormatPod => formatter.write_str("could not serialize synthetic format pod"),
            Self::Native(error) => write!(formatter, "PipeWire synthetic source error: {error}"),
        }
    }
}

impl std::error::Error for SyntheticSourceError {}

impl From<pipewire::Error> for SyntheticSourceError {
    fn from(error: pipewire::Error) -> Self {
        Self::Native(error)
    }
}

#[derive(Debug, Default)]
struct SyntheticCounters {
    callbacks: AtomicU64,
    frames: AtomicU64,
    missing_data: AtomicU64,
}

/// Lock-free source-side counters exposed to the integration control plane.
#[derive(Clone, Debug, Default)]
pub struct SyntheticSourceTelemetry {
    counters: Arc<SyntheticCounters>,
}

impl SyntheticSourceTelemetry {
    /// Returns source process-callback invocations.
    #[must_use]
    pub fn callbacks(&self) -> u64 {
        self.counters.callbacks.load(Ordering::Relaxed)
    }

    /// Returns deterministic source frames produced.
    #[must_use]
    pub fn frames(&self) -> u64 {
        self.counters.frames.load(Ordering::Relaxed)
    }

    /// Returns callbacks whose mapped output storage was unavailable.
    #[must_use]
    pub fn missing_data(&self) -> u64 {
        self.counters.missing_data.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
struct SignalGenerator {
    previous: f32,
    current: f32,
    recurrence: f32,
    noise_state: u32,
    telemetry: SyntheticSourceTelemetry,
}

impl SignalGenerator {
    fn new(telemetry: SyntheticSourceTelemetry) -> Self {
        let radians = std::f32::consts::TAU * TONE_HERTZ / SYNTHETIC_SOURCE_RATE_F32;
        Self {
            previous: 0.0,
            current: radians.sin(),
            recurrence: 2.0 * radians.cos(),
            noise_state: 0x6d2b_79f5,
            telemetry,
        }
    }

    fn next(&mut self) -> f32 {
        let tone = self.current;
        let next = self.recurrence.mul_add(self.current, -self.previous);
        self.previous = self.current;
        self.current = next;
        self.noise_state = self
            .noise_state
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        let upper = u16::try_from(self.noise_state >> 16).unwrap_or(u16::MAX);
        let noise = (f32::from(upper) / 32_768.0) - 1.0;
        TONE_AMPLITUDE.mul_add(tone, NOISE_AMPLITUDE * noise)
    }
}

/// Connected deterministic 44.1 kHz mono source for native session tests.
pub struct SyntheticSource {
    _listener: stream::StreamListener<SignalGenerator>,
    stream: StreamRc,
    telemetry: SyntheticSourceTelemetry,
    node_name: String,
    stream_error: Rc<RefCell<Option<String>>>,
}

impl SyntheticSource {
    /// Creates a source that emits deterministic 997 Hz tone plus seeded noise.
    ///
    /// # Errors
    ///
    /// Returns format serialization or native stream errors.
    pub fn connect(
        connection: &PipewireConnection,
        node_name: impl Into<String>,
    ) -> Result<Self, SyntheticSourceError> {
        let node_name = node_name.into();
        let properties = properties! {
            *keys::MEDIA_TYPE => "Audio",
            *keys::MEDIA_CATEGORY => "Capture",
            *keys::MEDIA_ROLE => "Communication",
            "media.class" => "Audio/Source",
            *keys::NODE_NAME => node_name.as_str(),
            *keys::NODE_DESCRIPTION => "Noire deterministic 44.1 kHz microphone",
            *keys::NODE_NICK => "Noire integration microphone",
            *keys::NODE_VIRTUAL => "false",
            "device.serial" => "noire-integration-44100",
            "device.name" => "noire-integration-device",
            "device.api" => "test",
            "audio.rate" => "44100",
            "audio.channels" => "1",
            "audio.position" => "[ MONO ]",
        };
        let stream = StreamRc::new(
            connection.core_clone(),
            "noire-synthetic-source",
            properties,
        )?;
        let telemetry = SyntheticSourceTelemetry::default();
        let stream_error = Rc::new(RefCell::new(None));
        let error_slot = Rc::clone(&stream_error);
        let listener = stream
            .add_local_listener_with_user_data(SignalGenerator::new(telemetry.clone()))
            .state_changed(move |_stream, _generator, _old, new| {
                if let stream::StreamState::Error(message) = new {
                    *error_slot.borrow_mut() = Some(message);
                }
            })
            .process(fill_available_buffers)
            .register()?;

        let pod_bytes = build_raw_audio_format_pod(SYNTHETIC_SOURCE_RATE)
            .map_err(|_| SyntheticSourceError::FormatPod)?;
        let pod = Pod::from_bytes(&pod_bytes).ok_or(SyntheticSourceError::FormatPod)?;
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
            telemetry,
            node_name,
            stream_error,
        })
    }

    /// Returns the stable node name used by target selection.
    #[must_use]
    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    /// Returns the fixed graph-facing fixture rate.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        SYNTHETIC_SOURCE_RATE
    }

    /// Returns lock-free source callback counters.
    #[must_use]
    pub fn telemetry(&self) -> SyntheticSourceTelemetry {
        self.telemetry.clone()
    }

    /// Removes the latest source stream error for control-plane reporting.
    #[must_use]
    pub fn take_error(&self) -> Option<String> {
        self.stream_error.borrow_mut().take()
    }

    /// Returns the transient node ID for graph assertions only.
    #[must_use]
    pub fn node_id(&self) -> u32 {
        self.stream.node_id()
    }
}

fn fill_available_buffers(stream: &pipewire::stream::Stream, generator: &mut SignalGenerator) {
    generator
        .telemetry
        .counters
        .callbacks
        .fetch_add(1, Ordering::Relaxed);
    while let Some(mut buffer) = stream.dequeue_buffer() {
        let datas = buffer.datas_mut();
        let Some(data) = datas.first_mut() else {
            generator
                .telemetry
                .counters
                .missing_data
                .fetch_add(1, Ordering::Relaxed);
            continue;
        };
        if data.as_raw().chunk.is_null() {
            generator
                .telemetry
                .counters
                .missing_data
                .fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let Some(bytes) = data.data() else {
            generator
                .telemetry
                .counters
                .missing_data
                .fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let frame_count = bytes.len() / BYTES_PER_SAMPLE;
        for frame in bytes.chunks_exact_mut(BYTES_PER_SAMPLE) {
            frame.copy_from_slice(&generator.next().to_ne_bytes());
        }
        let byte_count = frame_count.saturating_mul(BYTES_PER_SAMPLE);
        let chunk = data.chunk_mut();
        *chunk.offset_mut() = 0;
        *chunk.size_mut() = u32::try_from(byte_count).unwrap_or(u32::MAX);
        *chunk.stride_mut() = i32::try_from(BYTES_PER_SAMPLE).unwrap_or(i32::MAX);
        generator.telemetry.counters.frames.fetch_add(
            u64::try_from(frame_count).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{SignalGenerator, SyntheticSourceTelemetry};

    #[test]
    fn deterministic_signal_has_stable_finite_prefix() {
        let mut first = SignalGenerator::new(SyntheticSourceTelemetry::default());
        let mut second = SignalGenerator::new(SyntheticSourceTelemetry::default());
        for _ in 0..512 {
            let left = first.next();
            let right = second.next();
            assert_eq!(left.to_bits(), right.to_bits());
            assert!(left.is_finite());
            assert!(left.abs() < 0.25);
        }
    }
}
