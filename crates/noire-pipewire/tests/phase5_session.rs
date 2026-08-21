//! Live `FastEnhancer-B` acceptance in a disposable `PipeWire` graph.

#![cfg(feature = "native-test")]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]

use std::{
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use noire_model::DenoiserFactory;
use noire_model_fastenhancer::FastEnhancerFactory;
use noire_pipewire::{
    CaptureSink, CaptureStreamState, ConsumerDemand, DeadlinePolicy, InputGeneration, LiveGraph,
    LiveState, NativeCaptureStream, PipewireConnection, SourceStreamState, SyntheticSource,
    SyntheticSourceSpec,
};
use rtrb::{Consumer, Producer, RingBuffer};

const SELECTED_SOURCE: &str = "noire.integration.phase5.selected";
const SESSION_TIMEOUT: Duration = Duration::from_secs(10);
const RECORDING_CAPACITY: usize = 1_048_576;
const CORRELATION_TRIALS: usize = 100;
const CORRELATION_WINDOW: usize = 512;
const MAX_DELAY_SAMPLES: usize = 960;
const MAX_SEARCH_DELAY_SAMPLES: usize = 2_048;

#[derive(Debug)]
struct RecordingSink {
    producer: Producer<f32>,
    dropped: Arc<AtomicU64>,
}

impl CaptureSink for RecordingSink {
    fn write(&mut self, _generation: InputGeneration, samples: &[f32]) {
        let (_, remainder) = self.producer.push_partial_slice(samples);
        self.dropped.fetch_add(
            u64::try_from(remainder.len()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}

#[test]
#[ignore = "requires a disposable native PipeWire and WirePlumber session"]
fn live_fastenhancer_graph_meets_latency_demand_and_transport_gates() -> Result<(), Box<dyn Error>>
{
    let connection = PipewireConnection::connect_default()?;
    let selected = SyntheticSource::connect_with_spec(
        &connection,
        SELECTED_SOURCE,
        SyntheticSourceSpec {
            sample_rate: 48_000,
            tone_hertz: 1_000.0,
            tone_amplitude: 0.18,
            sequence_amplitude: 0.025,
            impulse_period_frames: 48_000,
            impulse_amplitude: 0.5,
            sequence_seed: 0x5a17,
        },
    )?;
    wait_until(&connection, SESSION_TIMEOUT, || {
        connection
            .registry_snapshot_now()
            .candidates()
            .iter()
            .any(|node| node.node_name == SELECTED_SOURCE)
    })?;

    let factory = FastEnhancerFactory::new()?;
    let graph = LiveGraph::connect(&connection, selected.node_name(), factory.create()?)?;
    graph.control().set_strength(0.0);
    graph.control().set_diagnostic_timing(true);
    wait_graph(
        &connection,
        &graph,
        SESSION_TIMEOUT,
        "initial pause",
        || graph.source().state() == SourceStreamState::Paused,
    )?;
    assert_eq!(graph.demand(), ConsumerDemand::Idle);
    assert_ne!(graph.capture().state(), CaptureStreamState::Streaming);

    graph.set_meter_monitoring(true)?;
    wait_graph(
        &connection,
        &graph,
        SESSION_TIMEOUT,
        "meter capture",
        || {
            graph.capture().state() == CaptureStreamState::Streaming
                && graph.telemetry().snapshot().model_frames > 20
        },
    )?;
    let meter_deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < meter_deadline {
        let _ = connection.dispatch_once(Duration::from_millis(1));
        let _ = graph.service_demand(Instant::now())?;
    }
    let meter_preview = graph.telemetry().snapshot();
    assert!(meter_preview.peak > 0.0);
    assert!(meter_preview.rms > 0.0);
    assert_eq!(meter_preview.state, LiveState::Running);
    assert_eq!(meter_preview.transport.overflows, 0);
    graph.set_meter_monitoring(false)?;
    wait_graph(&connection, &graph, SESSION_TIMEOUT, "meter idle", || {
        graph.capture().state() != CaptureStreamState::Streaming
    })?;

    let (reference_sink, mut reference_ring, reference_dropped) = recording_channel();
    let reference_capture = NativeCaptureStream::connect_with_sink(
        &connection,
        selected.node_name(),
        reference_sink,
        true,
    )?;
    let (output_sink, mut output_ring, output_dropped) = recording_channel();
    let output_capture = NativeCaptureStream::connect_with_sink(
        &connection,
        graph.source().node_name(),
        output_sink,
        true,
    )?;
    wait_graph(
        &connection,
        &graph,
        SESSION_TIMEOUT,
        "consumer activation",
        || {
            let _ = graph.service_demand(Instant::now());
            graph.demand() == ConsumerDemand::Active
                && graph.capture().state() == CaptureStreamState::Streaming
                && reference_capture.state() == CaptureStreamState::Streaming
                && output_capture.state() == CaptureStreamState::Streaming
        },
    )?;

    drain_discard(&mut reference_ring);
    drain_discard(&mut output_ring);
    let mut reference = Vec::with_capacity(320_000);
    let mut observed = Vec::with_capacity(320_000);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let _ = connection.dispatch_once(Duration::from_millis(1));
        let _ = graph.service_demand(Instant::now())?;
        drain_into(&mut reference_ring, &mut reference, 320_000);
        drain_into(&mut output_ring, &mut observed, 320_000);
        assert!(connection.take_failure().is_none());
        assert!(selected.take_error().is_none());
        assert!(graph.source().take_error().is_none());
    }
    assert_eq!(reference_dropped.load(Ordering::Relaxed), 0);
    assert_eq!(output_dropped.load(Ordering::Relaxed), 0);
    assert!(reference.len() > 120_000);
    assert!(observed.len() > 120_000);

    let delays = correlation_delays(&reference, &observed)?;
    let p95_samples = percentile(&delays, 0.95);
    assert!(
        p95_samples <= MAX_DELAY_SAMPLES,
        "live added p95 was {p95_samples} samples"
    );
    let bypass_snapshot = graph.telemetry().snapshot();
    assert_eq!(bypass_snapshot.state, LiveState::Running);
    assert_eq!(bypass_snapshot.model_errors, 0);
    assert!(
        bypass_snapshot.deadline_misses < u64::from(DeadlinePolicy::default().misses_per_window)
    );
    assert_eq!(bypass_snapshot.transport.underflows, 0);
    assert_eq!(bypass_snapshot.transport.overflows, 0);
    assert_eq!(bypass_snapshot.transport.dropped_frames, 0);

    graph.control().set_strength(1.0);
    let processed_deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < processed_deadline {
        let _ = connection.dispatch_once(Duration::from_millis(1));
        let _ = graph.service_demand(Instant::now())?;
        drain_discard(&mut reference_ring);
        drain_discard(&mut output_ring);
    }
    let live = graph.telemetry().snapshot();
    assert_eq!(live.state, LiveState::Running);
    assert!(live.model_frames > 500);
    assert!(live.model_timing.samples > 500);
    assert!(live.callback_timing.samples > 500);
    assert!(live.peak > 0.0);
    assert!(live.rms > 0.0);
    assert_eq!(live.model_errors, 0);
    assert_eq!(live.transport.underflows, 0);
    assert_eq!(live.transport.overflows, 0);

    drop(output_capture);
    wait_graph(
        &connection,
        &graph,
        SESSION_TIMEOUT,
        "consumer release",
        || {
            let _ = graph.service_demand(Instant::now());
            graph.demand() == ConsumerDemand::Idle
                && graph.capture().state() != CaptureStreamState::Streaming
        },
    )?;
    let idle_frames = graph.capture().telemetry().snapshot().counters.frames;
    let idle_deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < idle_deadline {
        let _ = connection.dispatch_once(Duration::from_millis(10));
        let _ = graph.service_demand(Instant::now())?;
    }
    assert_eq!(
        graph.capture().telemetry().snapshot().counters.frames,
        idle_frames
    );

    println!(
        "NOIRE_PHASE5_NATIVE latency_trials={CORRELATION_TRIALS} latency_p95_samples={p95_samples} latency_p95_ms={:.3} model_frames={} model_p99_ns={} model_max_ns={} callback_p99_ns={} callback_max_ns={} peak={:.6} rms={:.6} vad={:.6}",
        p95_samples as f64 * 1_000.0 / 48_000.0,
        live.model_frames,
        live.model_timing.percentile_ns(99),
        live.model_timing.maximum_ns,
        live.callback_timing.percentile_ns(99),
        live.callback_timing.maximum_ns,
        live.peak,
        live.rms,
        live.vad_probability,
    );
    Ok(())
}

#[test]
#[ignore = "requires a disposable native PipeWire and WirePlumber session"]
fn virtual_source_carries_noise_reduced_audio() -> Result<(), Box<dyn Error>> {
    let connection = PipewireConnection::connect_default()?;
    let selected = SyntheticSource::connect_with_spec(
        &connection,
        "noire.integration.noise-reduction.selected",
        SyntheticSourceSpec {
            sample_rate: 48_000,
            tone_hertz: 120.0,
            tone_amplitude: 0.04,
            sequence_amplitude: 0.08,
            impulse_period_frames: 0,
            impulse_amplitude: 0.0,
            sequence_seed: 0x5a17,
        },
    )?;
    wait_until(&connection, SESSION_TIMEOUT, || {
        connection
            .registry_snapshot_now()
            .candidates()
            .iter()
            .any(|node| node.node_name == selected.node_name())
    })?;

    let factory = FastEnhancerFactory::new()?;
    let graph = LiveGraph::connect(&connection, selected.node_name(), factory.create()?)?;
    graph.control().set_strength(0.55);
    let (reference_sink, mut reference_ring, reference_dropped) = recording_channel();
    let reference_capture = NativeCaptureStream::connect_with_sink(
        &connection,
        selected.node_name(),
        reference_sink,
        true,
    )?;
    let (output_sink, mut output_ring, output_dropped) = recording_channel();
    let output_capture = NativeCaptureStream::connect_with_sink(
        &connection,
        graph.source().node_name(),
        output_sink,
        true,
    )?;

    wait_graph(
        &connection,
        &graph,
        SESSION_TIMEOUT,
        "noise-reduction capture",
        || {
            graph.demand() == ConsumerDemand::Active
                && graph.capture().state() == CaptureStreamState::Streaming
                && reference_capture.state() == CaptureStreamState::Streaming
                && output_capture.state() == CaptureStreamState::Streaming
                && graph.telemetry().snapshot().model_frames > 40
        },
    )?;
    drain_discard(&mut reference_ring);
    drain_discard(&mut output_ring);

    let mut reference = Vec::with_capacity(192_000);
    let mut observed = Vec::with_capacity(192_000);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let _ = connection.dispatch_once(Duration::from_millis(1));
        let _ = graph.service_demand(Instant::now())?;
        drain_into(&mut reference_ring, &mut reference, 192_000);
        drain_into(&mut output_ring, &mut observed, 192_000);
    }

    assert_eq!(reference_dropped.load(Ordering::Relaxed), 0);
    assert_eq!(output_dropped.load(Ordering::Relaxed), 0);
    assert!(reference.len() > 96_000);
    assert!(observed.len() > 96_000);
    let input_rms = signal_rms(&reference);
    let output_rms = signal_rms(&observed);
    let attenuation_db = 20.0 * (input_rms / output_rms).log10();
    let live = graph.telemetry().snapshot();
    assert_eq!(live.state, LiveState::Running);
    assert_eq!(live.model_errors, 0);
    assert!(live.model_frames > 100);
    assert!(
        attenuation_db >= 1.0,
        "virtual-source attenuation was only {attenuation_db:.3} dB"
    );
    println!(
        "NOIRE_VIRTUAL_SOURCE_NOISE_REDUCTION input_rms={input_rms:.6} output_rms={output_rms:.6} attenuation_db={attenuation_db:.3} model_frames={} model_errors={}",
        live.model_frames, live.model_errors
    );
    Ok(())
}

fn recording_channel() -> (RecordingSink, Consumer<f32>, Arc<AtomicU64>) {
    let (producer, consumer) = RingBuffer::new(RECORDING_CAPACITY);
    let dropped = Arc::new(AtomicU64::new(0));
    (
        RecordingSink {
            producer,
            dropped: Arc::clone(&dropped),
        },
        consumer,
        dropped,
    )
}

fn wait_until(
    connection: &PipewireConnection,
    timeout: Duration,
    mut predicate: impl FnMut() -> bool,
) -> Result<(), &'static str> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let _ = connection.dispatch_once(Duration::from_millis(10));
        if predicate() {
            return Ok(());
        }
    }
    Err("timed out waiting for PipeWire state")
}

fn wait_graph(
    connection: &PipewireConnection,
    graph: &LiveGraph,
    timeout: Duration,
    phase: &'static str,
    mut predicate: impl FnMut() -> bool,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let _ = connection.dispatch_once(Duration::from_millis(10));
        let _ = graph.service_demand(Instant::now())?;
        if predicate() {
            return Ok(());
        }
    }
    Err(format!("timed out waiting for live PipeWire graph during {phase}").into())
}

fn drain_into(consumer: &mut Consumer<f32>, destination: &mut Vec<f32>, limit: usize) {
    while destination.len() < limit {
        match consumer.pop() {
            Ok(sample) => destination.push(sample),
            Err(_) => break,
        }
    }
}

fn drain_discard(consumer: &mut Consumer<f32>) {
    while consumer.pop().is_ok() {}
}

fn signal_rms(samples: &[f32]) -> f64 {
    let mean_square = samples
        .iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum::<f64>()
        / samples.len() as f64;
    mean_square.sqrt()
}

fn correlation_delays(reference: &[f32], observed: &[f32]) -> Result<Vec<usize>, &'static str> {
    let usable = reference.len().min(observed.len());
    let trial_stride =
        usable.saturating_sub(CORRELATION_WINDOW + MAX_SEARCH_DELAY_SAMPLES) / CORRELATION_TRIALS;
    if trial_stride < CORRELATION_WINDOW {
        return Err("insufficient audio for latency trials");
    }
    let mut delays = Vec::with_capacity(CORRELATION_TRIALS);
    for trial in 0..CORRELATION_TRIALS {
        let start = trial * trial_stride;
        let reference_window = &reference[start..start + CORRELATION_WINDOW];
        let mut best_delay = 0;
        let mut best_score = f64::NEG_INFINITY;
        for delay in 1..=MAX_SEARCH_DELAY_SAMPLES {
            let observed_window = &observed[start + delay..start + delay + CORRELATION_WINDOW];
            let score = normalized_correlation(reference_window, observed_window);
            if score > best_score {
                best_score = score;
                best_delay = delay;
            }
        }
        if best_score < 0.8 {
            return Err("latency correlation was inconclusive");
        }
        delays.push(best_delay);
    }
    Ok(delays)
}

fn normalized_correlation(left: &[f32], right: &[f32]) -> f64 {
    let left_mean = left.iter().map(|sample| f64::from(*sample)).sum::<f64>() / left.len() as f64;
    let right_mean =
        right.iter().map(|sample| f64::from(*sample)).sum::<f64>() / right.len() as f64;
    let mut dot = 0.0;
    let mut left_energy = 0.0;
    let mut right_energy = 0.0;
    for (left, right) in left.iter().zip(right.iter()) {
        let left = f64::from(*left) - left_mean;
        let right = f64::from(*right) - right_mean;
        dot += left * right;
        left_energy += left * left;
        right_energy += right * right;
    }
    if left_energy <= f64::EPSILON || right_energy <= f64::EPSILON {
        return f64::NEG_INFINITY;
    }
    dot / (left_energy * right_energy).sqrt()
}

fn percentile(samples: &[usize], quantile: f64) -> usize {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() - 1) as f64 * quantile).ceil() as usize;
    sorted[index]
}
