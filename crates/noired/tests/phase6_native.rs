//! Native daemon-to-PipeWire lifecycle composition acceptance.

#![cfg(feature = "native-test")]

use std::{
    error::Error,
    time::{Duration, Instant},
};

use noire_config::{Config, InputConfig, InputMode, SuppressionConfig};
use noire_pipewire::{
    CaptureStreamState, NativeCaptureStream, PipewireConnection, RESERVED_NODE_NAME,
    SyntheticSource,
};
use noired::{AudioEngine, NativeAudioEngine};

const SELECTED_SOURCE: &str = "noire.integration.phase6.selected";
const TIMEOUT: Duration = Duration::from_secs(10);

#[test]
#[ignore = "requires a disposable native PipeWire and WirePlumber session"]
fn daemon_engine_creates_controls_and_removes_the_live_graph() -> Result<(), Box<dyn Error>> {
    let fixture_connection = PipewireConnection::connect_default()?;
    let fixture = SyntheticSource::connect(&fixture_connection, SELECTED_SOURCE)?;
    wait_until(&fixture_connection, || {
        fixture_connection
            .registry_snapshot_now()
            .candidates()
            .iter()
            .any(|node| node.node_name == SELECTED_SOURCE)
    })?;

    let mut engine = NativeAudioEngine::spawn()?;
    let inputs = engine.inputs()?;
    assert!(
        inputs
            .iter()
            .any(|input| input.stable_id == SELECTED_SOURCE)
    );

    let mut config = Config {
        active: true,
        input: InputConfig {
            mode: InputMode::Selected,
            stable_id: SELECTED_SOURCE.to_owned(),
            ..InputConfig::default()
        },
        suppression: SuppressionConfig {
            strength: 0.35,
            ..SuppressionConfig::default()
        },
        ..Config::default()
    };
    let running = engine.apply(&config)?;
    assert_eq!(running.state.to_string(), "running");
    assert_eq!(
        running.input_display_name,
        "Noire deterministic 44.1 kHz microphone"
    );
    let output_probe = NativeCaptureStream::connect(&fixture_connection, RESERVED_NODE_NAME)?;
    wait_until(&fixture_connection, || {
        output_probe.state() == CaptureStreamState::Streaming
            && output_probe.telemetry().snapshot().counters.frames > 4_800
    })?;

    config.suppression.enabled = false;
    let bypass = engine.apply(&config)?;
    assert_eq!(bypass.state.to_string(), "running");
    assert_eq!(bypass.input_display_name, running.input_display_name);

    drop(output_probe);
    config.active = false;
    let stopped = engine.apply(&config)?;
    assert_eq!(stopped.state.to_string(), "stopped");
    assert_eq!(stopped.input_display_name, running.input_display_name);
    assert!(!stopped.metrics.vad_probability.is_nan());

    engine.set_meter_monitoring(true)?;
    let meter_deadline = Instant::now() + TIMEOUT;
    let monitored = loop {
        let _ = fixture_connection.dispatch_once(Duration::from_millis(5));
        let observation = engine.observe(&stopped)?;
        if observation.metrics.peak > 0.0 && observation.metrics.rms > 0.0 {
            break observation;
        }
        if Instant::now() >= meter_deadline {
            return Err("timed out waiting for stopped-state microphone meters".into());
        }
    };
    assert_eq!(monitored.state.to_string(), "stopped");
    assert_eq!(monitored.input_display_name, running.input_display_name);
    engine.set_meter_monitoring(false)?;
    drop(engine);
    assert!(fixture.take_error().is_none());

    println!(
        "NOIRE_PHASE6_NATIVE input={SELECTED_SOURCE} start=running smooth_control=pass stop=stopped"
    );
    Ok(())
}

fn wait_until(
    connection: &PipewireConnection,
    mut condition: impl FnMut() -> bool,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        let _ = connection.dispatch_once(Duration::from_millis(5));
        if condition() {
            return Ok(());
        }
    }
    Err("timed out waiting for native Phase-6 state".into())
}
