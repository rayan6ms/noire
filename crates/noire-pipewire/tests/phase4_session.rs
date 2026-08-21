//! Phase-4 end-to-end bypass and virtual-source acceptance in a disposable graph.

#![cfg(feature = "native-test")]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::struct_field_names,
    clippy::too_many_lines
)]

use std::{
    error::Error,
    fs::{self, File},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use noire_pipewire::{
    BypassGraph, BypassGraphService, CANONICAL_CAPTURE_FORMAT, CaptureSink, CaptureStreamState,
    ConsumerDemand, InputGeneration, NativeCaptureStream, NegotiatedFormatEvent,
    PipewireConnection, RESERVED_NODE_NAME, SourceStreamState, SyntheticSource,
    SyntheticSourceSpec,
};
use rtrb::{Consumer, Producer, RingBuffer};

const SELECTED_SOURCE: &str = "noire.integration.phase4.selected";
const UNSELECTED_SOURCE: &str = "noire.integration.phase4.unselected";
const SESSION_TIMEOUT: Duration = Duration::from_secs(10);
const RECORDING_CAPACITY: usize = 1_048_576;
const MEASUREMENT_SECONDS: u64 = 4;
const MAX_ADDED_DELAY_SAMPLES: usize = 960;
const CORRELATION_TRIALS: usize = 100;
const CORRELATION_WINDOW: usize = 512;
const APP_TIMEOUT: Duration = Duration::from_secs(25);

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
fn virtual_source_bypass_meets_phase4_acceptance() -> Result<(), Box<dyn Error>> {
    let connection = PipewireConnection::connect_default()?;
    let selected_spec = SyntheticSourceSpec {
        sample_rate: 48_000,
        tone_hertz: 1_000.0,
        tone_amplitude: 0.18,
        sequence_amplitude: 0.015,
        impulse_period_frames: 48_000,
        impulse_amplitude: 0.55,
        sequence_seed: 0xace1,
    };
    let selected = SyntheticSource::connect_with_spec(&connection, SELECTED_SOURCE, selected_spec)?;
    let unselected = SyntheticSource::connect_with_spec(
        &connection,
        UNSELECTED_SOURCE,
        SyntheticSourceSpec {
            tone_hertz: 431.0,
            tone_amplitude: 0.7,
            sequence_amplitude: 0.0,
            ..selected_spec
        },
    )?;
    wait_until(&connection, SESSION_TIMEOUT, || {
        let snapshot = connection.registry_snapshot_now();
        snapshot
            .candidates()
            .iter()
            .any(|node| node.node_name == SELECTED_SOURCE)
            && snapshot
                .candidates()
                .iter()
                .any(|node| node.node_name == UNSELECTED_SOURCE)
    })?;

    let graph = BypassGraph::connect(&connection, selected.node_name())?;
    wait_graph(&connection, &graph, SESSION_TIMEOUT, || {
        graph.source().state() == SourceStreamState::Paused
    })?;
    assert_eq!(graph.demand(), ConsumerDemand::Idle);
    assert_ne!(graph.capture().state(), CaptureStreamState::Streaming);
    assert!(
        connection
            .registry_snapshot_now()
            .candidates()
            .iter()
            .all(|node| node.node_name != RESERVED_NODE_NAME)
    );

    let source_node_id = graph.source().node_id();
    let initial_dump = pipewire_dump(&connection, Some(&graph), source_node_id)?;
    assert_eq!(node_name_occurrences(&initial_dump, RESERVED_NODE_NAME), 1);
    assert!(initial_dump.contains("\"node.description\": \"Noire Microphone ☾\""));
    assert!(initial_dump.contains("\"node.nick\": \"Noire\""));
    assert!(
        initial_dump.contains("\"node.virtual\": \"true\"")
            || initial_dump.contains("\"node.virtual\": true"),
        "virtual flag was absent from {initial_dump}"
    );
    assert!(initial_dump.contains("\"media.class\": \"Audio/Source\""));
    assert!(!initial_dump.contains("object.linger"));

    let idle_cpu_percentiles = measure_idle_cpu(&connection, &graph)?;
    assert!(
        idle_cpu_percentiles.p95 < 1.0,
        "idle p95 was {}%",
        idle_cpu_percentiles.p95
    );
    assert_eq!(selected.telemetry().frames(), 0);
    assert_eq!(unselected.telemetry().frames(), 0);

    let (reference_sink, mut reference_ring, reference_dropped) = recording_channel();
    let reference_capture = NativeCaptureStream::connect_with_sink(
        &connection,
        selected.node_name(),
        reference_sink,
        true,
    )?;
    wait_until(&connection, SESSION_TIMEOUT, || {
        reference_capture.state() == CaptureStreamState::Streaming
    })?;

    let (output_sink, mut output_ring, output_dropped) = recording_channel();
    let output_capture = NativeCaptureStream::connect_with_sink(
        &connection,
        graph.source().node_name(),
        output_sink,
        true,
    )?;
    let mut activation_seen = false;
    let mut source_format = None;
    wait_graph(&connection, &graph, SESSION_TIMEOUT, || {
        if let Some(event) = graph.source().take_negotiated_format() {
            source_format = Some(event);
        }
        activation_seen |= matches!(
            graph.service_demand(Instant::now()),
            Ok(BypassGraphService::Activated)
        );
        graph.demand() == ConsumerDemand::Active
            && graph.capture().state() == CaptureStreamState::Streaming
            && output_capture.state() == CaptureStreamState::Streaming
            && source_format.is_some()
    })?;
    assert!(activation_seen || graph.demand() == ConsumerDemand::Active);
    assert_eq!(
        source_format,
        Some(NegotiatedFormatEvent::Accepted(CANONICAL_CAPTURE_FORMAT))
    );

    drain_discard(&mut reference_ring);
    drain_discard(&mut output_ring);
    let mut reference = Vec::with_capacity(300_000);
    let mut observed = Vec::with_capacity(300_000);
    let measurement_deadline = Instant::now() + Duration::from_secs(MEASUREMENT_SECONDS);
    while Instant::now() < measurement_deadline {
        let _ = connection.dispatch_once(Duration::from_millis(10));
        let _ = graph.service_demand(Instant::now())?;
        drain_into(&mut reference_ring, &mut reference, 300_000);
        drain_into(&mut output_ring, &mut observed, 300_000);
        assert!(connection.take_failure().is_none());
        assert!(selected.take_error().is_none());
        assert!(unselected.take_error().is_none());
        assert!(graph.source().take_error().is_none());
    }
    assert_eq!(reference_dropped.load(Ordering::Relaxed), 0);
    assert_eq!(output_dropped.load(Ordering::Relaxed), 0);
    let measurement_transport = graph.telemetry().snapshot();
    let measurement_source = graph.source().telemetry().snapshot();
    let measurement_capture = output_capture.telemetry().snapshot();
    assert!(
        reference.len() >= 60_000,
        "reference captured only {} frames",
        reference.len()
    );
    assert!(
        observed.len() >= 60_000,
        "virtual source captured {} frames; transport={measurement_transport:?} source={measurement_source:?} capture={measurement_capture:?}",
        observed.len(),
    );

    let delays = correlation_delays(&reference, &observed)?;
    let latency = LatencySummary::from_samples(delays);
    assert!(latency.p95_samples <= u64::try_from(MAX_ADDED_DELAY_SAMPLES).unwrap_or(u64::MAX));
    assert!(latency.minimum_samples > 0);
    let gain_error_db = aligned_gain_error_db(
        &reference,
        &observed,
        usize::try_from(latency.median_samples).unwrap_or(MAX_ADDED_DELAY_SAMPLES),
    )?;
    assert!(
        gain_error_db.abs() <= 0.1,
        "gain error was {gain_error_db} dB"
    );
    assert!(observed.iter().all(|sample| sample.is_finite()));
    assert!(observed.iter().all(|sample| sample.abs() < 1.0));
    let impulse_matches = aligned_impulse_matches(
        &reference,
        &observed,
        usize::try_from(latency.median_samples).unwrap_or(MAX_ADDED_DELAY_SAMPLES),
    );
    assert!(
        impulse_matches >= 3,
        "only {impulse_matches} impulses aligned at delay {}; reference impulses {:?}; observed impulses {:?}",
        latency.median_samples,
        impulse_indices(&reference),
        impulse_indices(&observed),
    );

    let soak_started = Instant::now();
    let soak_deadline = soak_started + soak_duration();
    while Instant::now() < soak_deadline {
        // Production blocks in the event-driven main loop. A 1 ms finite
        // iterate preserves that wake-up behavior in this single-threaded
        // harness without spending half of NFR-001's budget asleep.
        let _ = connection.dispatch_once(Duration::from_millis(1));
        let _ = graph.service_demand(Instant::now())?;
        drain_discard(&mut reference_ring);
        drain_discard(&mut output_ring);
        assert_eq!(graph.demand(), ConsumerDemand::Active);
        assert!(connection.take_failure().is_none());
        assert!(selected.take_error().is_none());
        assert!(graph.source().take_error().is_none());
    }

    let transport = graph.telemetry().snapshot();
    let source_boundary = graph.source().telemetry().snapshot();
    assert!(transport.output_callbacks > 0);
    assert!(transport.output_frames > 0);
    assert_eq!(transport.underflows, 0);
    assert_eq!(transport.overflows, 0);
    assert_eq!(transport.dropped_frames, 0);
    assert_eq!(transport.missing_frames, 0);
    assert_eq!(transport.oversized_requests, 0);
    assert_eq!(transport.sanitized_samples, 0);
    assert_eq!(source_boundary.missing_buffers, 0);
    assert_eq!(source_boundary.malformed_buffers, 0);
    assert_eq!(unselected.telemetry().frames(), 0);

    drop(output_capture);
    wait_graph(&connection, &graph, SESSION_TIMEOUT, || {
        graph.demand() == ConsumerDemand::Idle
            && graph.capture().state() != CaptureStreamState::Streaming
    })?;
    let frames_at_idle = graph.capture().telemetry().snapshot().counters.frames;
    let idle_check_deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < idle_check_deadline {
        let _ = connection.dispatch_once(Duration::from_millis(10));
        let _ = graph.service_demand(Instant::now())?;
    }
    assert_eq!(
        graph.capture().telemetry().snapshot().counters.frames,
        frames_at_idle
    );

    let reconnect_capture = NativeCaptureStream::connect(&connection, graph.source().node_name())?;
    wait_graph(&connection, &graph, SESSION_TIMEOUT, || {
        graph.demand() == ConsumerDemand::Active
            && reconnect_capture.state() == CaptureStreamState::Streaming
            && graph.capture().state() == CaptureStreamState::Streaming
    })?;
    let reconnect_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < reconnect_deadline {
        let _ = connection.dispatch_once(Duration::from_millis(10));
        let _ = graph.service_demand(Instant::now())?;
    }
    let reconnect_snapshot = reconnect_capture.telemetry().snapshot();
    assert!(reconnect_snapshot.counters.frames > 48_000);
    assert!(reconnect_snapshot.peak > 0.1);
    assert_eq!(graph.telemetry().snapshot().underflows, 0);
    drop(reconnect_capture);
    drop(reference_capture);
    drop(graph);
    wait_until(&connection, SESSION_TIMEOUT, || {
        pipewire_dump(&connection, None, source_node_id)
            .is_ok_and(|dump| node_name_occurrences(&dump, RESERVED_NODE_NAME) == 0)
    })?;

    println!(
        "NOIRE_PHASE4_RESULT duration_ms={} idle_p95_percent={:.3} latency_trials={} latency_min_samples={} latency_median_samples={} latency_p95_samples={} latency_p99_samples={} latency_max_samples={} latency_p95_ms={:.3} gain_error_db={:.6} impulse_matches={} produced_frames={} output_frames={} startup_silence_frames={} high_water_frames={} generations={} selected_frames={} unselected_frames={}",
        soak_duration().as_millis(),
        idle_cpu_percentiles.p95,
        CORRELATION_TRIALS,
        latency.minimum_samples,
        latency.median_samples,
        latency.p95_samples,
        latency.p99_samples,
        latency.maximum_samples,
        latency.p95_samples as f64 * 1_000.0 / 48_000.0,
        gain_error_db,
        impulse_matches,
        transport.produced_frames,
        transport.output_frames,
        transport.startup_silence_frames,
        transport.high_water_frames,
        transport.generation,
        selected.telemetry().frames(),
        unselected.telemetry().frames(),
    );
    Ok(())
}

#[test]
#[ignore = "requires native PipeWire and pipewire-pulse compatibility tools"]
fn native_and_pulse_recorders_select_virtual_source() -> Result<(), Box<dyn Error>> {
    let connection = PipewireConnection::connect_default()?;
    let selected = SyntheticSource::connect_with_spec(
        &connection,
        "noire.integration.phase4.compat",
        SyntheticSourceSpec {
            sample_rate: 48_000,
            tone_hertz: 1_000.0,
            tone_amplitude: 0.2,
            sequence_amplitude: 0.01,
            ..SyntheticSourceSpec::default()
        },
    )?;
    wait_until(&connection, SESSION_TIMEOUT, || {
        connection
            .registry_snapshot_now()
            .candidates()
            .iter()
            .any(|node| node.node_name == selected.node_name())
    })?;
    let graph = BypassGraph::connect(&connection, selected.node_name())?;
    wait_graph(&connection, &graph, SESSION_TIMEOUT, || {
        matches!(
            graph.source().state(),
            SourceStreamState::Paused | SourceStreamState::Streaming
        )
    })?;
    let pulse_sources =
        command_output_with_graph(&connection, &graph, "pactl", &["list", "short", "sources"])?;
    assert!(pulse_sources.contains(RESERVED_NODE_NAME));

    let suffix = std::process::id();
    let native_path = format!("/tmp/noire-phase4-native-{suffix}.wav");
    let mut native = Command::new("pw-record")
        .args([
            "--target",
            RESERVED_NODE_NAME,
            "--rate=48000",
            "--channels=1",
            "--channel-map=mono",
            "--format=f32",
            "--latency=128",
            native_path.as_str(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    drive_recording_client(&connection, &graph, &mut native, Duration::from_secs(2))?;
    let native_bytes = fs::metadata(&native_path)?.len();
    assert!(native_bytes > 48_000 * 4);
    fs::remove_file(&native_path)?;
    wait_graph(&connection, &graph, SESSION_TIMEOUT, || {
        graph.demand() == ConsumerDemand::Idle
    })?;

    let pulse_path = format!("/tmp/noire-phase4-pulse-{suffix}.raw");
    let pulse_file = File::create(&pulse_path)?;
    let mut pulse = Command::new("parec")
        .args([
            "--device",
            RESERVED_NODE_NAME,
            "--rate=48000",
            "--channels=1",
            "--format=float32le",
            "--raw",
        ])
        .stdout(Stdio::from(pulse_file))
        .stderr(Stdio::null())
        .spawn()?;
    drive_recording_client(&connection, &graph, &mut pulse, Duration::from_secs(2))?;
    let pulse_bytes = fs::metadata(&pulse_path)?.len();
    assert!(
        pulse_bytes > 48_000,
        "Pulse compatibility recorder wrote only {pulse_bytes} bytes"
    );
    fs::remove_file(&pulse_path)?;

    let transport = graph.telemetry().snapshot();
    assert_eq!(transport.underflows, 0);
    assert_eq!(transport.overflows, 0);
    assert_eq!(transport.oversized_requests, 0);
    println!(
        "NOIRE_PHASE4_COMPAT_RESULT pactl_visible=true native_bytes={native_bytes} pulse_bytes={pulse_bytes} underflows={} overflows={}",
        transport.underflows, transport.overflows
    );
    Ok(())
}

#[test]
#[ignore = "requires installed Chrome, Electron, OBS, Xvfb, and pipewire-pulse"]
fn zz_application_clients_select_and_record_virtual_source() -> Result<(), Box<dyn Error>> {
    if std::env::var_os("NOIRE_PHASE4_APP_SMOKE").is_none() {
        println!("NOIRE_PHASE4_APP_RESULT skipped=true reason=NOIRE_PHASE4_APP_SMOKE-unset");
        return Ok(());
    }

    let connection = PipewireConnection::connect_default()?;
    let selected = SyntheticSource::connect_with_spec(
        &connection,
        "noire.integration.phase4.app-clients",
        SyntheticSourceSpec {
            sample_rate: 48_000,
            tone_hertz: 1_000.0,
            tone_amplitude: 0.2,
            sequence_amplitude: 0.01,
            ..SyntheticSourceSpec::default()
        },
    )?;
    wait_until(&connection, SESSION_TIMEOUT, || {
        connection
            .registry_snapshot_now()
            .candidates()
            .iter()
            .any(|node| node.node_name == selected.node_name())
    })?;
    let graph = connect_app_graph(&connection, selected.node_name())?;

    let suffix = std::process::id();
    let result_path = PathBuf::from(format!("/tmp/noire-webrtc-result-{suffix}.json"));
    remove_file_if_present(&result_path)?;
    let port = reserve_loopback_port()?;
    let repository = repository_root();
    let server_script = repository.join("tests/compat/webrtc-smoke-server.py");
    let mut server_command = Command::new("python3");
    server_command
        .arg(server_script)
        .arg(port.to_string())
        .env("NOIRE_WEBRTC_RESULT_PATH", &result_path)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let mut server = ChildGuard::spawn(&mut server_command)?;
    wait_for_server(&connection, &graph, &mut server, port)?;
    let url = format!("http://127.0.0.1:{port}/webrtc-smoke.html");

    let chrome_profile = format!("/tmp/noire-chrome-profile-{suffix}");
    let mut chrome_command = Command::new("setsid");
    chrome_command
        .arg("google-chrome")
        .args([
            "--no-sandbox",
            "--headless=new",
            "--use-fake-ui-for-media-stream",
            "--autoplay-policy=no-user-gesture-required",
            "--disable-gpu",
            "--disable-dev-shm-usage",
            "--no-first-run",
            "--no-default-browser-check",
            format!("--user-data-dir={chrome_profile}").as_str(),
            url.as_str(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let chrome_result = run_webrtc_client(
        &connection,
        &graph,
        &mut chrome_command,
        &result_path,
        "Chrome",
    )?;
    assert_transport_clean(graph.telemetry().snapshot());
    drop(graph);
    settle_after_graph_drop(&connection);

    let graph = connect_app_graph(&connection, selected.node_name())?;
    remove_file_if_present(&result_path)?;
    let mut electron_command = Command::new("setsid");
    electron_command
        .args([
            "xvfb-run",
            "-a",
            "/opt/noire-electron/node_modules/.bin/electron",
        ])
        .arg("--no-sandbox")
        .arg(repository.join("tests/compat/electron-main.cjs"))
        .arg(url.as_str())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let electron_result = run_webrtc_client(
        &connection,
        &graph,
        &mut electron_command,
        &result_path,
        "Electron",
    )?;
    assert_transport_clean(graph.telemetry().snapshot());
    drop(graph);
    settle_after_graph_drop(&connection);

    let graph = connect_app_graph(&connection, selected.node_name())?;
    let obs_frames = run_obs_client(&connection, &graph, suffix)?;
    let transport = graph.telemetry().snapshot();
    assert_transport_clean(transport);
    server.stop()?;
    remove_file_if_present(&result_path)?;

    println!(
        "NOIRE_PHASE4_APP_RESULT skipped=false chrome=pass electron=pass obs=pass chrome_result={} electron_result={} obs_frames={obs_frames} underflows={} overflows={}",
        compact_result(&chrome_result),
        compact_result(&electron_result),
        transport.underflows,
        transport.overflows,
    );
    Ok(())
}

fn connect_app_graph(
    connection: &PipewireConnection,
    selected_node_name: &str,
) -> Result<BypassGraph, Box<dyn Error>> {
    let graph = BypassGraph::connect(connection, selected_node_name)?;
    wait_graph(connection, &graph, SESSION_TIMEOUT, || {
        graph.source().state() == SourceStreamState::Paused
    })?;
    let _ = command_output_with_graph(
        connection,
        &graph,
        "pactl",
        &["set-default-source", RESERVED_NODE_NAME],
    )?;
    Ok(graph)
}

fn settle_after_graph_drop(connection: &PipewireConnection) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        let _ = connection.dispatch_once(Duration::from_millis(10));
    }
}

fn assert_transport_clean(transport: noire_pipewire::BypassTelemetrySnapshot) {
    assert_eq!(transport.underflows, 0);
    assert_eq!(transport.overflows, 0);
    assert_eq!(transport.missing_frames, 0);
    assert_eq!(transport.oversized_requests, 0);
}

struct ChildGuard {
    child: Option<std::process::Child>,
}

impl ChildGuard {
    fn spawn(command: &mut Command) -> Result<Self, std::io::Error> {
        Ok(Self {
            child: Some(command.spawn()?),
        })
    }

    fn child_mut(&mut self) -> Result<&mut std::process::Child, &'static str> {
        self.child.as_mut().ok_or("child process already stopped")
    }

    fn id(&self) -> Result<u32, &'static str> {
        self.child
            .as_ref()
            .map(std::process::Child::id)
            .ok_or("child process already stopped")
    }

    fn stop(&mut self) -> Result<(), std::io::Error> {
        if let Some(mut child) = self.child.take() {
            if child.try_wait()?.is_none() {
                child.kill()?;
            }
            let _ = child.wait()?;
        }
        Ok(())
    }

    fn stop_process_group(&mut self) -> Result<(), std::io::Error> {
        if let Some(child) = &self.child {
            let group = format!("-{}", child.id());
            let _ = Command::new("kill")
                .args(["-KILL", "--", group.as_str()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
        }
        self.stop()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn reserve_loopback_port() -> Result<u16, std::io::Error> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

fn wait_for_server(
    connection: &PipewireConnection,
    graph: &BypassGraph,
    server: &mut ChildGuard,
    port: u16,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + SESSION_TIMEOUT;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
        if let Some(status) = server.child_mut()?.try_wait()? {
            return Err(format!("WebRTC fixture server exited early with {status}").into());
        }
        let _ = connection.dispatch_once(Duration::from_millis(10));
        let _ = graph.service_demand(Instant::now())?;
    }
    Err("WebRTC fixture server did not listen".into())
}

fn run_webrtc_client(
    connection: &PipewireConnection,
    graph: &BypassGraph,
    command: &mut Command,
    result_path: &Path,
    client_name: &str,
) -> Result<String, Box<dyn Error>> {
    let before = graph.telemetry().snapshot();
    let mut client = ChildGuard::spawn(command)?;
    let deadline = Instant::now() + APP_TIMEOUT;
    let result = loop {
        if result_path.is_file() {
            break fs::read_to_string(result_path)?;
        }
        if let Some(status) = client.child_mut()?.try_wait()? {
            return Err(format!("{client_name} exited before reporting with {status}").into());
        }
        let _ = connection.dispatch_once(Duration::from_millis(10));
        let _ = graph.service_demand(Instant::now())?;
        if Instant::now() >= deadline {
            return Err(format!("{client_name} WebRTC smoke timed out").into());
        }
    };
    let grace_deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < grace_deadline && client.child_mut()?.try_wait()?.is_none() {
        let _ = connection.dispatch_once(Duration::from_millis(10));
        let _ = graph.service_demand(Instant::now())?;
    }
    client.stop_process_group()?;
    let after = graph.telemetry().snapshot();
    assert!(
        result.contains("\"pass\":true")
            && result.contains("Noire Microphone ☾")
            && result.contains("\"recordedBytes\":"),
        "{client_name} rejected the virtual microphone: {result}"
    );
    assert!(
        after.produced_frames > before.produced_frames
            && after.output_frames > before.output_frames,
        "{client_name} selected Noire but did not drive its audio graph"
    );
    Ok(result)
}

fn run_obs_client(
    connection: &PipewireConnection,
    graph: &BypassGraph,
    suffix: u32,
) -> Result<u64, Box<dyn Error>> {
    let fixture_root = repository_root().join("tests/compat/obs");
    let config_root = PathBuf::from(format!("/tmp/noire-obs-config-{suffix}"));
    let profile = config_root.join("obs-studio/basic/profiles/Noire");
    let scenes = config_root.join("obs-studio/basic/scenes");
    let recording_root = PathBuf::from(format!("/tmp/noire-obs-recordings-{suffix}"));
    fs::create_dir_all(&profile)?;
    fs::create_dir_all(&scenes)?;
    fs::create_dir_all(&recording_root)?;
    fs::copy(
        fixture_root.join("global.ini"),
        config_root.join("obs-studio/global.ini"),
    )?;
    let basic = fs::read_to_string(fixture_root.join("basic.ini"))?.replace(
        "/tmp/noire-obs-recordings",
        recording_root.to_string_lossy().as_ref(),
    );
    fs::write(profile.join("basic.ini"), basic)?;
    fs::copy(fixture_root.join("Noire.json"), scenes.join("Noire.json"))?;

    let log_path = PathBuf::from(format!("/tmp/noire-obs-{suffix}.log"));
    let log = File::create(&log_path)?;
    let mut obs_command = Command::new("setsid");
    obs_command
        .args([
            "xvfb-run",
            "-a",
            "obs",
            "--startrecording",
            "--disable-shutdown-check",
            "--disable-missing-files-check",
            "--safe-mode",
            "--multi",
            "--collection",
            "Noire",
            "--profile",
            "Noire",
            "--scene",
            "Scene",
            "--verbose",
        ])
        .env("XDG_CONFIG_HOME", &config_root)
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    let mut obs = ChildGuard::spawn(&mut obs_command)?;
    let active_deadline = Instant::now() + APP_TIMEOUT;
    while graph.demand() != ConsumerDemand::Active {
        if let Some(status) = obs.child_mut()?.try_wait()? {
            return Err(format!("OBS exited before selecting Noire with {status}").into());
        }
        let _ = connection.dispatch_once(Duration::from_millis(10));
        let _ = graph.service_demand(Instant::now())?;
        if Instant::now() >= active_deadline {
            return Err(format!(
                "OBS did not select Noire; log={}",
                fs::read_to_string(&log_path)?
            )
            .into());
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(status) = obs.child_mut()?.try_wait()? {
            return Err(format!("OBS exited while recording with {status}").into());
        }
        let _ = connection.dispatch_once(Duration::from_millis(10));
        let _ = graph.service_demand(Instant::now())?;
    }
    let obs_pid = obs.id()?.to_string();
    let interrupt = Command::new("pkill")
        .args(["-INT", "-P", obs_pid.as_str(), "-x", "obs"])
        .status()?;
    assert!(
        interrupt.success(),
        "could not send OBS a graceful interrupt"
    );
    let stop_deadline = Instant::now() + SESSION_TIMEOUT;
    while Instant::now() < stop_deadline && obs.child_mut()?.try_wait()?.is_none() {
        let _ = connection.dispatch_once(Duration::from_millis(10));
        let _ = graph.service_demand(Instant::now())?;
    }
    obs.stop_process_group()?;
    let obs_log = fs::read_to_string(&log_path)?;
    assert!(
        obs_log.contains(RESERVED_NODE_NAME) || obs_log.contains("Noire Microphone ☾"),
        "OBS log did not name the configured source: {obs_log}"
    );
    let recorded_frames = parse_obs_recorded_frames(&obs_log).unwrap_or(0);
    assert!(
        recorded_frames > 48_000,
        "OBS did not capture a full second from Noire; log={obs_log}"
    );
    Ok(recorded_frames)
}

fn parse_obs_recorded_frames(log: &str) -> Option<u64> {
    log.lines().find_map(|line| {
        let (_, suffix) = line.split_once(" packets with ")?;
        let (frames, _) = suffix.split_once(" frames")?;
        frames.parse().ok()
    })
}

fn remove_file_if_present(path: &Path) -> Result<(), std::io::Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn compact_result(result: &str) -> String {
    result
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
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
    mut condition: impl FnMut() -> bool,
) -> Result<(), &'static str> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let _ = connection.dispatch_once(Duration::from_millis(10));
        if condition() {
            return Ok(());
        }
        thread::yield_now();
    }
    Err("native PipeWire condition timed out")
}

fn wait_graph(
    connection: &PipewireConnection,
    graph: &BypassGraph,
    timeout: Duration,
    mut condition: impl FnMut() -> bool,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let _ = connection.dispatch_once(Duration::from_millis(10));
        let _ = graph.service_demand(Instant::now())?;
        if condition() {
            return Ok(());
        }
        thread::yield_now();
    }
    Err(format!(
        "native PipeWire graph condition timed out: source_state={:?} demand={:?} capture_state={:?} source_error={:?} core_failure={:?} transport={:?} source={:?}",
        graph.source().state(),
        graph.demand(),
        graph.capture().state(),
        graph.source().take_error(),
        connection.take_failure(),
        graph.telemetry().snapshot(),
        graph.source().telemetry().snapshot(),
    )
    .into())
}

fn drain_into(consumer: &mut Consumer<f32>, destination: &mut Vec<f32>, limit: usize) {
    while destination.len() < limit {
        let Ok(sample) = consumer.pop() else {
            break;
        };
        destination.push(sample);
    }
    if destination.len() == limit {
        drain_discard(consumer);
    }
}

fn drain_discard(consumer: &mut Consumer<f32>) {
    while consumer.pop().is_ok() {}
}

fn soak_duration() -> Duration {
    std::env::var("NOIRE_PHASE4_SOAK_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map_or(Duration::from_secs(5), Duration::from_secs)
}

fn pipewire_dump(
    connection: &PipewireConnection,
    graph: Option<&BypassGraph>,
    object_id: u32,
) -> Result<String, Box<dyn Error>> {
    let mut child = Command::new("pw-dump")
        .arg("-N")
        .arg(object_id.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + SESSION_TIMEOUT;
    while child.try_wait()?.is_none() {
        let _ = connection.dispatch_once(Duration::from_millis(10));
        if let Some(graph) = graph {
            let _ = graph.service_demand(Instant::now())?;
        }
        if Instant::now() >= deadline {
            child.kill()?;
            return Err("pw-dump timed out".into());
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err("pw-dump failed".into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn command_output_with_graph(
    connection: &PipewireConnection,
    graph: &BypassGraph,
    program: &str,
    arguments: &[&str],
) -> Result<String, Box<dyn Error>> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + SESSION_TIMEOUT;
    while child.try_wait()?.is_none() {
        let _ = connection.dispatch_once(Duration::from_millis(10));
        let _ = graph.service_demand(Instant::now())?;
        if Instant::now() >= deadline {
            child.kill()?;
            return Err(format!("{program} timed out").into());
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!("{program} failed").into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn drive_recording_client(
    connection: &PipewireConnection,
    graph: &BypassGraph,
    child: &mut std::process::Child,
    duration: Duration,
) -> Result<(), Box<dyn Error>> {
    let active_deadline = Instant::now() + SESSION_TIMEOUT;
    while graph.demand() != ConsumerDemand::Active {
        if let Some(status) = child.try_wait()? {
            return Err(format!("recording client exited early with {status}").into());
        }
        let _ = connection.dispatch_once(Duration::from_millis(10));
        let _ = graph.service_demand(Instant::now())?;
        if Instant::now() >= active_deadline {
            child.kill()?;
            return Err("recording client did not activate source".into());
        }
    }
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            return Err(format!("recording client exited early with {status}").into());
        }
        let _ = connection.dispatch_once(Duration::from_millis(10));
        let _ = graph.service_demand(Instant::now())?;
    }
    child.kill()?;
    let _ = child.wait()?;
    Ok(())
}

fn node_name_occurrences(dump: &str, node_name: &str) -> usize {
    dump.match_indices(&format!("\"node.name\": \"{node_name}\""))
        .count()
}

#[derive(Clone, Copy, Debug)]
struct CpuPercentiles {
    p95: f64,
}

fn measure_idle_cpu(
    connection: &PipewireConnection,
    graph: &BypassGraph,
) -> Result<CpuPercentiles, Box<dyn Error>> {
    let windows = std::env::var("NOIRE_PHASE4_IDLE_WINDOWS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|windows| *windows >= 2)
        .unwrap_or(5);
    let ticks_per_second = clock_ticks_per_second()?;
    let mut samples = Vec::with_capacity(windows);
    for _ in 0..windows {
        let ticks_before = process_cpu_ticks()?;
        let started = Instant::now();
        let deadline = started + Duration::from_secs(1);
        while Instant::now() < deadline {
            let _ = connection.dispatch_once(Duration::from_millis(10));
            assert_eq!(
                graph.service_demand(Instant::now())?,
                BypassGraphService::Unchanged
            );
        }
        let elapsed = started.elapsed().as_secs_f64();
        let ticks = process_cpu_ticks()?.saturating_sub(ticks_before);
        samples.push(ticks as f64 * 100.0 / ticks_per_second as f64 / elapsed);
    }
    samples.sort_by(f64::total_cmp);
    Ok(CpuPercentiles {
        p95: percentile(&samples, 0.95),
    })
}

fn clock_ticks_per_second() -> Result<u64, Box<dyn Error>> {
    let output = Command::new("getconf").arg("CLK_TCK").output()?;
    if !output.status.success() {
        return Err("getconf CLK_TCK failed".into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().parse()?)
}

fn process_cpu_ticks() -> Result<u64, Box<dyn Error>> {
    let stat = fs::read_to_string("/proc/self/stat")?;
    let fields = stat
        .get(stat.rfind(')').ok_or("malformed /proc/self/stat")? + 2..)
        .ok_or("malformed /proc/self/stat")?
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    let user: u64 = fields.get(11).ok_or("missing utime")?.parse()?;
    let system: u64 = fields.get(12).ok_or("missing stime")?.parse()?;
    Ok(user.saturating_add(system))
}

#[derive(Clone, Copy, Debug)]
struct LatencySummary {
    minimum_samples: u64,
    median_samples: u64,
    p95_samples: u64,
    p99_samples: u64,
    maximum_samples: u64,
}

impl LatencySummary {
    fn from_samples(mut samples: Vec<usize>) -> Self {
        samples.sort_unstable();
        Self {
            minimum_samples: percentile_usize(&samples, 0.0) as u64,
            median_samples: percentile_usize(&samples, 0.5) as u64,
            p95_samples: percentile_usize(&samples, 0.95) as u64,
            p99_samples: percentile_usize(&samples, 0.99) as u64,
            maximum_samples: percentile_usize(&samples, 1.0) as u64,
        }
    }
}

fn correlation_delays(reference: &[f32], observed: &[f32]) -> Result<Vec<usize>, &'static str> {
    let warmup = 4_096;
    let needed = warmup
        + CORRELATION_TRIALS * CORRELATION_WINDOW
        + MAX_ADDED_DELAY_SAMPLES
        + CORRELATION_WINDOW;
    if reference.len() < needed || observed.len() < needed {
        return Err("insufficient samples for correlation trials");
    }
    let mut delays = Vec::with_capacity(CORRELATION_TRIALS);
    for trial in 0..CORRELATION_TRIALS {
        let start = warmup + trial * CORRELATION_WINDOW;
        let mut best_delay = 0;
        let mut best_score = f64::NEG_INFINITY;
        for delay in 0..=MAX_ADDED_DELAY_SAMPLES {
            let score = normalized_correlation(
                &reference[start..start + CORRELATION_WINDOW],
                &observed[start + delay..start + delay + CORRELATION_WINDOW],
            );
            if score > best_score {
                best_score = score;
                best_delay = delay;
            }
        }
        if best_score < 0.9 {
            return Err("bypass correlation was below 0.9");
        }
        delays.push(best_delay);
    }
    Ok(delays)
}

fn normalized_correlation(left: &[f32], right: &[f32]) -> f64 {
    let left_mean = left.iter().map(|sample| f64::from(*sample)).sum::<f64>() / left.len() as f64;
    let right_mean =
        right.iter().map(|sample| f64::from(*sample)).sum::<f64>() / right.len() as f64;
    let mut cross = 0.0;
    let mut left_energy = 0.0;
    let mut right_energy = 0.0;
    for (left, right) in left.iter().zip(right) {
        let left = f64::from(*left) - left_mean;
        let right = f64::from(*right) - right_mean;
        cross += left * right;
        left_energy += left * left;
        right_energy += right * right;
    }
    cross / (left_energy * right_energy).sqrt().max(f64::EPSILON)
}

fn aligned_gain_error_db(
    reference: &[f32],
    observed: &[f32],
    delay: usize,
) -> Result<f64, &'static str> {
    let length = reference.len().min(observed.len().saturating_sub(delay));
    let mut reference_energy = 0.0;
    let mut observed_energy = 0.0;
    let mut samples = 0_u64;
    for (reference, observed) in reference[..length]
        .iter()
        .zip(&observed[delay..delay + length])
    {
        if reference.abs() > 0.4 || observed.abs() > 0.4 {
            continue;
        }
        reference_energy += f64::from(*reference).powi(2);
        observed_energy += f64::from(*observed).powi(2);
        samples = samples.saturating_add(1);
    }
    if samples == 0 || reference_energy <= 0.0 || observed_energy <= 0.0 {
        return Err("no finite aligned energy");
    }
    Ok(20.0 * (observed_energy / reference_energy).sqrt().log10())
}

fn aligned_impulse_matches(reference: &[f32], observed: &[f32], delay: usize) -> usize {
    reference
        .iter()
        .enumerate()
        .filter(|(_, sample)| **sample > 0.4)
        .filter(|(index, _)| {
            let expected = index.saturating_add(delay);
            if expected >= observed.len() {
                return false;
            }
            let start = expected.saturating_sub(2);
            let end = expected.saturating_add(3).min(observed.len());
            observed[start..end].iter().any(|sample| *sample > 0.4)
        })
        .count()
}

fn impulse_indices(samples: &[f32]) -> Vec<usize> {
    samples
        .iter()
        .enumerate()
        .filter_map(|(index, sample)| (*sample > 0.4).then_some(index))
        .take(8)
        .collect()
}

fn percentile(samples: &[f64], quantile: f64) -> f64 {
    samples[percentile_index(samples.len(), quantile)]
}

fn percentile_usize(samples: &[usize], quantile: f64) -> usize {
    samples[percentile_index(samples.len(), quantile)]
}

fn percentile_index(length: usize, quantile: f64) -> usize {
    ((quantile * length as f64).ceil() as usize)
        .saturating_sub(1)
        .min(length.saturating_sub(1))
}
