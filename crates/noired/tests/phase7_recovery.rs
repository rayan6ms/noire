//! Native recovery-cycle and bounded-command acceptance.

#![cfg(feature = "native-test")]

use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use noire_config::{Config, InputConfig, InputMode, SuppressionConfig};
use noire_pipewire::{
    PipewireConnection, RESERVED_NODE_NAME, SyntheticSource, SyntheticSourceSpec,
};
use noired::{AudioEngine, LifecycleState, NativeAudioEngine};

const SOURCE_NAME: &str = "noire.integration.phase7.recovery";
const ALTERNATE_SOURCE_NAME: &str = "noire.integration.phase7.default-alternate";
const HOTPLUG_CYCLES: u32 = 100;
const RESTART_CYCLES: u32 = 20;
const INPUT_RECOVERY_LIMIT: Duration = Duration::from_secs(2);
const CORE_RECOVERY_LIMIT: Duration = Duration::from_secs(3);
const COMMAND_LIMIT: Duration = Duration::from_millis(500);
const RSS_GROWTH_LIMIT_KIB: u64 = 5 * 1_024;

struct NativeSession {
    pipewire: Child,
    wireplumber: Child,
    socket: PathBuf,
}

impl NativeSession {
    fn start(runtime: &Path) -> Result<Self, Box<dyn Error>> {
        let socket = runtime.join("pipewire-0");
        remove_runtime_socket_files(&socket);
        let pipewire = Command::new("pipewire")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        wait_for(CORE_RECOVERY_LIMIT, || socket.exists())?;
        let wireplumber = Command::new("wireplumber")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        thread::sleep(Duration::from_millis(150));
        Ok(Self {
            pipewire,
            wireplumber,
            socket,
        })
    }

    fn stop(mut self) -> Result<(), Box<dyn Error>> {
        terminate(&mut self.wireplumber)?;
        terminate(&mut self.pipewire)?;
        remove_runtime_socket_files(&self.socket);
        Ok(())
    }

    fn pause(&self) -> Result<(), Box<dyn Error>> {
        signal(self.pipewire.id(), "STOP")
    }

    fn resume(&self) -> Result<(), Box<dyn Error>> {
        signal(self.pipewire.id(), "CONT")
    }
}

impl Drop for NativeSession {
    fn drop(&mut self) {
        let _ = self.wireplumber.kill();
        let _ = self.wireplumber.wait();
        let _ = self.pipewire.kill();
        let _ = self.pipewire.wait();
    }
}

#[test]
#[ignore = "requires an isolated D-Bus session with PipeWire and WirePlumber binaries"]
#[allow(clippy::too_many_lines)]
fn native_recovery_survives_hotplug_restarts_and_command_storms() -> Result<(), Box<dyn Error>> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or("XDG_RUNTIME_DIR is required")?;
    let mut session = Some(NativeSession::start(&runtime)?);
    let connection = PipewireConnection::connect_default()?;
    let mut source = SyntheticSource::connect(&connection, SOURCE_NAME)?;
    let alternate_source = SyntheticSource::connect(&connection, ALTERNATE_SOURCE_NAME)?;
    wait_for_source(&connection, SOURCE_NAME, true)?;
    wait_for_source(&connection, ALTERNATE_SOURCE_NAME, true)?;
    set_default_source(&connection, SOURCE_NAME)?;

    let mut engine = NativeAudioEngine::spawn()?;
    let mut config = selected_config();
    let initial = engine.apply(&config)?;
    assert_eq!(initial.state, LifecycleState::Running);
    assert_single_virtual_source(&connection)?;
    let steady_rss_kib = resident_set_kib().unwrap_or_default();
    eprintln!("phase7-recovery: initial graph ready");

    set_default_source(&connection, ALTERNATE_SOURCE_NAME)?;
    wait_for_engine_default(&mut engine, ALTERNATE_SOURCE_NAME)?;
    config.input.mode = InputMode::FollowDefault;
    config.input.stable_id.clear();
    assert_eq!(engine.apply(&config)?.state, LifecycleState::Running);
    set_default_source(&connection, SOURCE_NAME)?;
    wait_for_engine_default(&mut engine, SOURCE_NAME)?;
    assert_eq!(
        wait_for_engine(&mut engine, INPUT_RECOVERY_LIMIT)?.state,
        LifecycleState::Running
    );
    config = selected_config();
    assert_eq!(engine.apply(&config)?.state, LifecycleState::Running);
    drop(alternate_source);
    wait_for_source(&connection, ALTERNATE_SOURCE_NAME, false)?;
    eprintln!("phase7-recovery: default changes ready");

    let active_session = session.as_ref().ok_or("native session disappeared")?;
    active_session.pause()?;
    thread::sleep(Duration::from_millis(100));
    let suspended_command = Instant::now();
    let _ = engine.inputs();
    assert!(suspended_command.elapsed() < COMMAND_LIMIT);
    active_session.resume()?;
    assert_eq!(
        wait_for_engine(&mut engine, CORE_RECOVERY_LIMIT)?.state,
        LifecycleState::Running
    );
    eprintln!("phase7-recovery: pause/resume ready");

    let mut pressure = Command::new("stress-ng")
        .args([
            "--cpu",
            "2",
            "--vm",
            "1",
            "--vm-bytes",
            "256M",
            "--timeout",
            "10s",
            "--metrics-brief",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let mut input_recovery_micros = Vec::with_capacity(HOTPLUG_CYCLES as usize);
    for cycle in 0..HOTPLUG_CYCLES {
        drop(source);
        wait_for_source(&connection, SOURCE_NAME, false)?;
        let command_started = Instant::now();
        let _ = engine.inputs();
        assert!(command_started.elapsed() < COMMAND_LIMIT);
        let degraded =
            wait_for_engine_state(&mut engine, LifecycleState::Degraded, INPUT_RECOVERY_LIMIT)?;
        assert_eq!(degraded.state, LifecycleState::Degraded);
        assert!(degraded.fault.is_some());

        let spec = SyntheticSourceSpec {
            sample_rate: if cycle % 2 == 0 { 48_000 } else { 44_100 },
            ..SyntheticSourceSpec::default()
        };
        source = SyntheticSource::connect_with_spec(&connection, SOURCE_NAME, spec)?;
        wait_for_source(&connection, SOURCE_NAME, true)?;
        let recovery_started = Instant::now();
        let recovered = wait_for_engine(&mut engine, INPUT_RECOVERY_LIMIT)?;
        assert_eq!(recovered.state, LifecycleState::Running);
        assert_single_virtual_source(&connection)?;
        input_recovery_micros.push(micros(recovery_started.elapsed()));
        if cycle % 10 == 9 {
            eprintln!("phase7-recovery: hotplug cycles={}", cycle + 1);
        }
    }
    assert!(pressure.wait()?.success());

    drop(source);
    drop(connection);
    if let Some(active) = session.take() {
        active.stop()?;
    }
    let degraded = wait_for_engine_fault(&mut engine, "pipewire-unavailable", CORE_RECOVERY_LIMIT)?;
    assert_eq!(degraded.state, LifecycleState::Degraded);
    assert_eq!(
        degraded.fault.as_ref().map(|fault| fault.code),
        Some("pipewire-unavailable")
    );

    let mut core_recovery_micros = Vec::with_capacity(RESTART_CYCLES as usize);
    for cycle in 0..RESTART_CYCLES {
        let command_started = Instant::now();
        let _ = engine.inputs();
        assert!(command_started.elapsed() < COMMAND_LIMIT);

        let active = NativeSession::start(&runtime)?;
        let fixture = PipewireConnection::connect_default()?;
        let fixture_source = SyntheticSource::connect(&fixture, SOURCE_NAME)?;
        wait_for_source(&fixture, SOURCE_NAME, true)?;
        let recovery_started = Instant::now();
        let recovered = wait_for_engine(&mut engine, CORE_RECOVERY_LIMIT)?;
        assert_eq!(recovered.state, LifecycleState::Running);
        assert_single_virtual_source(&fixture)?;
        core_recovery_micros.push(micros(recovery_started.elapsed()));

        for command in 0..50 {
            config.suppression.enabled = command % 2 == 0;
            config.suppression.strength = f64::from(command) / 50.0;
            let command_started = Instant::now();
            let result = engine.apply(&config)?;
            assert_eq!(result.state, LifecycleState::Running);
            assert!(command_started.elapsed() < COMMAND_LIMIT);
        }
        drop(fixture_source);
        drop(fixture);
        active.stop()?;
        let degraded =
            wait_for_engine_fault(&mut engine, "pipewire-unavailable", CORE_RECOVERY_LIMIT)?;
        assert_eq!(degraded.state, LifecycleState::Degraded);
        assert_eq!(
            degraded.fault.as_ref().map(|fault| fault.code),
            Some("pipewire-unavailable")
        );
        eprintln!("phase7-recovery: restart cycles={}", cycle + 1);
    }

    config.active = false;
    let stopped = engine.apply(&config)?;
    assert_eq!(stopped.state, LifecycleState::Stopped);
    let rss_growth_kib = resident_set_kib()
        .unwrap_or(steady_rss_kib)
        .saturating_sub(steady_rss_kib);
    assert!(rss_growth_kib < RSS_GROWTH_LIMIT_KIB);

    input_recovery_micros.sort_unstable();
    core_recovery_micros.sort_unstable();
    let input_p95_us = percentile(&input_recovery_micros, 95);
    let core_p95_us = percentile(&core_recovery_micros, 95);
    assert!(input_p95_us < micros(INPUT_RECOVERY_LIMIT));
    assert!(core_p95_us < micros(CORE_RECOVERY_LIMIT));
    println!(
        "NOIRE_PHASE7_RECOVERY hotplug_cycles={HOTPLUG_CYCLES} pipewire_restarts={RESTART_CYCLES} default_changes=2 format_changes={HOTPLUG_CYCLES} input_p95_us={input_p95_us} core_p95_us={core_p95_us} rss_growth_kib={rss_growth_kib} duplicate_sources=0 command_storm=1000"
    );
    Ok(())
}

fn selected_config() -> Config {
    Config {
        active: true,
        input: InputConfig {
            mode: InputMode::Selected,
            stable_id: SOURCE_NAME.to_owned(),
            ..InputConfig::default()
        },
        suppression: SuppressionConfig {
            strength: 0.5,
            ..SuppressionConfig::default()
        },
        ..Config::default()
    }
}

fn wait_for_engine(
    engine: &mut NativeAudioEngine,
    timeout: Duration,
) -> Result<noired::EngineObservation, Box<dyn Error>> {
    wait_for_engine_state(engine, LifecycleState::Running, timeout)
}

fn wait_for_engine_state(
    engine: &mut NativeAudioEngine,
    state: LifecycleState,
    timeout: Duration,
) -> Result<noired::EngineObservation, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut last_error = None;
    while Instant::now() < deadline {
        match engine.observe(&noired::EngineObservation::default()) {
            Ok(observation) if observation.state == state => {
                return Ok(observation);
            }
            Ok(_) => {}
            Err(error) => last_error = Some(error),
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(last_error.map_or_else(
        || format!("native engine did not reach {state}").into(),
        |error| Box::new(error) as Box<dyn Error>,
    ))
}

fn wait_for_engine_fault(
    engine: &mut NativeAudioEngine,
    code: &str,
    timeout: Duration,
) -> Result<noired::EngineObservation, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut last_error = None;
    while Instant::now() < deadline {
        match engine.observe(&noired::EngineObservation::default()) {
            Ok(observation)
                if observation.state == LifecycleState::Degraded
                    && observation.fault.as_ref().map(|fault| fault.code) == Some(code) =>
            {
                return Ok(observation);
            }
            Ok(_) => {}
            Err(error) => last_error = Some(error),
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(last_error.map_or_else(
        || format!("native engine did not report {code}").into(),
        |error| Box::new(error) as Box<dyn Error>,
    ))
}

fn wait_for_source(
    connection: &PipewireConnection,
    name: &str,
    present: bool,
) -> Result<(), Box<dyn Error>> {
    wait_for(CORE_RECOVERY_LIMIT, || {
        let _ = connection.dispatch_once(Duration::from_millis(5));
        connection
            .registry_snapshot_now()
            .candidates()
            .iter()
            .any(|candidate| candidate.node_name == name)
            == present
    })
}

fn set_default_source(connection: &PipewireConnection, name: &str) -> Result<(), Box<dyn Error>> {
    let present = connection
        .registry_snapshot_now()
        .candidates()
        .iter()
        .any(|candidate| candidate.node_name == name);
    if !present {
        return Err("default-source fixture is absent".into());
    }
    let value = format!(r#"{{"name":"{name}"}}"#);
    let mut metadata = Command::new("pw-metadata")
        .args([
            "-n",
            "default",
            "0",
            "default.audio.source",
            value.as_str(),
            "Spa:String:JSON",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let result = wait_for(INPUT_RECOVERY_LIMIT, || {
        let _ = connection.dispatch_once(Duration::from_millis(5));
        connection
            .registry_snapshot_now()
            .candidates()
            .iter()
            .any(|candidate| candidate.node_name == name && candidate.is_default)
    });
    let _ = metadata.kill();
    let _ = metadata.wait();
    result
}

fn wait_for_engine_default(
    engine: &mut NativeAudioEngine,
    name: &str,
) -> Result<(), Box<dyn Error>> {
    wait_for(INPUT_RECOVERY_LIMIT, || {
        engine.inputs().is_ok_and(|inputs| {
            inputs
                .iter()
                .any(|input| input.stable_id == name && input.is_default)
        })
    })
}

fn wait_for(timeout: Duration, mut condition: impl FnMut() -> bool) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(5));
    }
    Err("native Phase-7 condition timed out".into())
}

fn terminate(child: &mut Child) -> Result<(), Box<dyn Error>> {
    child.kill()?;
    let _ = child.wait()?;
    Ok(())
}

fn remove_runtime_socket_files(socket: &Path) {
    let _ = fs::remove_file(socket);
    let _ = fs::remove_file(socket.with_extension("lock"));
    if let Some(name) = socket.file_name().and_then(|name| name.to_str()) {
        let manager = socket.with_file_name(format!("{name}-manager"));
        let _ = fs::remove_file(&manager);
        let _ = fs::remove_file(manager.with_extension("lock"));
    }
}

fn signal(pid: u32, name: &str) -> Result<(), Box<dyn Error>> {
    let status = Command::new("kill")
        .args([format!("-{name}"), pid.to_string()])
        .status()?;
    if !status.success() {
        return Err(format!("could not send SIG{name} to PipeWire").into());
    }
    Ok(())
}

fn resident_set_kib() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))?
        .split_ascii_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let index = sorted.len().saturating_mul(percentile).saturating_sub(1) / 100;
    sorted.get(index).copied().unwrap_or_default()
}

fn assert_single_virtual_source(connection: &PipewireConnection) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + INPUT_RECOVERY_LIMIT;
    loop {
        let _ = connection.dispatch_once(Duration::from_millis(5));
        let occurrences = connection
            .registry_snapshot_now()
            .node_name_occurrences(RESERVED_NODE_NAME);
        if occurrences == 1 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("expected one Noire virtual source, found {occurrences}").into());
        }
        thread::sleep(Duration::from_millis(5));
    }
}
