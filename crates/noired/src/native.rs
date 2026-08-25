//! Bounded command adapter owning all `PipeWire` objects on one native thread.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use noire_config::{Config, FailMode as ConfigFailMode, InputMode, LatencyProfile};
use noire_ipc::{InputDescriptor, Metrics};
use noire_model::DenoiserFactory;
use noire_model_fastenhancer::FastEnhancerFactory;
use noire_pipewire::{
    DeviceAvailability, FailMode, GraphHealthIssue, InputResolution, LiveGraph, LiveState,
    PipewireConnection, RegistrySnapshot, SelectionPolicy, StreamLatency,
};

use crate::{
    AudioEngine, EngineError, EngineObservation, LifecycleState, RecoveryController, RecoveryFault,
};

const COMMAND_CAPACITY: usize = 16;
const COMMAND_TIMEOUT: Duration = Duration::from_millis(450);
const DISPATCH_QUANTUM: Duration = Duration::from_millis(2);
/// Minimum spacing between latched live-failure deactivated resets.
///
/// A latched [`LiveState::ModelFailed`] or [`LiveState::TransportFailed`] sink
/// stays silent until its documented deactivated reset runs. The interval
/// bounds the reset rate so a persistently failing model cannot turn the
/// control loop into a hot reset cycle while remaining fast enough to restore
/// audio within a fraction of a second after a transient stall.
const LIVE_FAILURE_RESET_INTERVAL: Duration = Duration::from_millis(250);

enum NativeCommand {
    Apply {
        config: Config,
        cancelled: Arc<AtomicBool>,
        reply: SyncSender<Result<EngineObservation, EngineError>>,
    },
    Inputs {
        cancelled: Arc<AtomicBool>,
        reply: SyncSender<Result<Vec<InputDescriptor>, EngineError>>,
    },
    Observe {
        cancelled: Arc<AtomicBool>,
        reply: SyncSender<Result<EngineObservation, EngineError>>,
    },
    SetMeterMonitoring {
        enabled: bool,
        cancelled: Arc<AtomicBool>,
        reply: SyncSender<Result<(), EngineError>>,
    },
    Shutdown,
}

/// Fixed-capacity daemon-to-PipeWire command endpoint.
pub struct NativeAudioEngine {
    commands: Option<SyncSender<NativeCommand>>,
    shutdown_complete: Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for NativeAudioEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeAudioEngine")
            .finish_non_exhaustive()
    }
}

impl NativeAudioEngine {
    /// Spawns the single thread that owns `PipeWire` and the live graph.
    ///
    /// # Errors
    ///
    /// Returns a stable failure if the operating system cannot create the thread.
    pub fn spawn() -> Result<Self, EngineError> {
        let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (shutdown_complete, shutdown_wait) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("noire-pipewire".to_owned())
            .spawn(move || run_native(&receiver, &shutdown_complete))
            .map_err(|error| EngineError {
                code: "audio-thread-unavailable",
                message: format!("could not create the audio control thread: {error}"),
                recovery: "free process resources and restart Noire",
                retryable: true,
            })?;
        Ok(Self {
            commands: Some(commands),
            shutdown_complete: shutdown_wait,
            thread: Some(thread),
        })
    }

    fn request<T>(
        &self,
        build: impl FnOnce(SyncSender<Result<T, EngineError>>, Arc<AtomicBool>) -> NativeCommand,
    ) -> Result<T, EngineError> {
        self.request_with_timeout(build, COMMAND_TIMEOUT)
    }

    fn request_with_timeout<T>(
        &self,
        build: impl FnOnce(SyncSender<Result<T, EngineError>>, Arc<AtomicBool>) -> NativeCommand,
        timeout: Duration,
    ) -> Result<T, EngineError> {
        let (reply, receive) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        self.commands
            .as_ref()
            .ok_or_else(stopped_error)?
            .try_send(build(reply, Arc::clone(&cancelled)))
            .map_err(command_error)?;
        match receive.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // A timed-out command may still be sitting in the bounded FIFO.
                // Mark it before returning so the native owner skips stale
                // work instead of applying it after the caller has retried.
                cancelled.store(true, Ordering::Release);
                Err(EngineError {
                    code: "audio-command-timeout",
                    message: "the audio control thread did not respond within 450 ms".to_owned(),
                    recovery: "retry; restart Noire if the condition persists",
                    retryable: true,
                })
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                cancelled.store(true, Ordering::Release);
                Err(stopped_error())
            }
        }
    }
}

impl AudioEngine for NativeAudioEngine {
    fn apply(&mut self, config: &Config) -> Result<EngineObservation, EngineError> {
        self.request(|reply, cancelled| NativeCommand::Apply {
            config: config.clone(),
            cancelled,
            reply,
        })
    }

    fn inputs(&mut self) -> Result<Vec<InputDescriptor>, EngineError> {
        self.request(|reply, cancelled| NativeCommand::Inputs { cancelled, reply })
    }

    fn observe(&mut self, _previous: &EngineObservation) -> Result<EngineObservation, EngineError> {
        self.request(|reply, cancelled| NativeCommand::Observe { cancelled, reply })
    }

    fn set_meter_monitoring(&mut self, enabled: bool) -> Result<(), EngineError> {
        self.request(|reply, cancelled| NativeCommand::SetMeterMonitoring {
            enabled,
            cancelled,
            reply,
        })
    }
}

impl Drop for NativeAudioEngine {
    fn drop(&mut self) {
        if let Some(commands) = self.commands.take() {
            let _ = commands.try_send(NativeCommand::Shutdown);
            drop(commands);
        }
        if self.shutdown_complete.recv_timeout(COMMAND_TIMEOUT).is_ok()
            && let Some(thread) = self.thread.take()
        {
            let _ = thread.join();
        }
    }
}

fn stopped_error() -> EngineError {
    EngineError {
        code: "audio-thread-stopped",
        message: "the audio control thread has stopped".to_owned(),
        recovery: "restart the Noire daemon",
        retryable: false,
    }
}

#[allow(clippy::needless_pass_by_value)]
fn command_error(error: TrySendError<NativeCommand>) -> EngineError {
    match error {
        TrySendError::Full(_) => EngineError {
            code: "audio-command-busy",
            message: "the bounded audio command queue is full".to_owned(),
            recovery: "wait briefly and retry the command",
            retryable: true,
        },
        TrySendError::Disconnected(_) => EngineError {
            code: "audio-thread-stopped",
            message: "the audio control thread has stopped".to_owned(),
            recovery: "restart the Noire daemon",
            retryable: false,
        },
    }
}

#[allow(clippy::too_many_lines)]
fn run_native(receiver: &Receiver<NativeCommand>, shutdown_complete: &SyncSender<()>) {
    let mut connection: Option<PipewireConnection> = None;
    let mut graph: Option<LiveGraph> = None;
    let mut applied: Option<Config> = None;
    let mut observation: Option<EngineObservation> = None;
    let mut meter_monitoring = false;
    let mut last_live_failure_reset: Option<Instant> = None;
    let recovery_origin = Instant::now();
    let mut recovery = RecoveryController::default();
    loop {
        match receiver.recv_timeout(DISPATCH_QUANTUM) {
            Ok(NativeCommand::Apply {
                config,
                cancelled,
                reply,
            }) => {
                if cancelled.load(Ordering::Acquire) {
                    continue;
                }
                let previous_config = applied.clone();
                let previous_observation = observation.clone();
                let result = apply_native(
                    &mut connection,
                    &mut graph,
                    previous_config.as_ref(),
                    previous_observation.as_ref(),
                    &config,
                    meter_monitoring,
                );
                if let Ok(applied_observation) = result.as_ref() {
                    applied = Some(config);
                    observation = Some(applied_observation.clone());
                    if applied.as_ref().is_some_and(|config| config.active) {
                        recovery.recovered();
                    } else {
                        recovery.stop();
                    }
                } else if let Some(previous_config) = previous_config.as_ref()
                    && let Ok(restored) = apply_native(
                        &mut connection,
                        &mut graph,
                        None,
                        previous_observation.as_ref(),
                        previous_config,
                        meter_monitoring,
                    )
                {
                    observation = Some(restored);
                    recovery.recovered();
                } else if previous_config.is_none() && config.active {
                    applied = Some(config);
                    observation = Some(degraded_observation(
                        previous_observation.as_ref(),
                        result.as_ref().err(),
                    ));
                    let fault = result
                        .as_ref()
                        .err()
                        .map_or(RecoveryFault::Core, classify_initial_fault);
                    recovery.fault(fault, elapsed_millis(recovery_origin));
                }
                let _ = reply.send(result);
            }
            Ok(NativeCommand::Inputs { cancelled, reply }) => {
                if cancelled.load(Ordering::Acquire) {
                    continue;
                }
                let result = ensure_connection(&mut connection).map(|connection| {
                    refresh_registry(connection);
                    input_descriptors(&connection.registry_snapshot_now())
                });
                if let Ok(devices) = result.as_ref() {
                    cache_observed_devices(&mut observation, devices);
                }
                let _ = reply.send(result);
            }
            Ok(NativeCommand::Observe { cancelled, reply }) => {
                if cancelled.load(Ordering::Acquire) {
                    continue;
                }
                if let (Some(connection), Some(graph), Some(previous)) =
                    (connection.as_ref(), graph.as_ref(), observation.as_ref())
                {
                    let mut next = observe_graph(
                        graph,
                        previous.input_display_name.clone(),
                        connection.runtime_version().unwrap_or_default(),
                        applied.as_ref().is_some_and(|config| config.active),
                    );
                    next.devices.clone_from(&previous.devices);
                    observation = Some(next);
                }
                let response = observation
                    .get_or_insert_with(EngineObservation::default)
                    .clone();
                let _ = reply.send(Ok(response));
            }
            Ok(NativeCommand::SetMeterMonitoring {
                enabled,
                cancelled,
                reply,
            }) => {
                if cancelled.load(Ordering::Acquire) {
                    continue;
                }
                let result = update_meter_monitoring(
                    &mut connection,
                    &mut graph,
                    applied.as_ref(),
                    &mut observation,
                    enabled,
                );
                if result.is_ok() {
                    meter_monitoring = enabled;
                }
                let _ = reply.send(result);
            }
            Ok(NativeCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        service_native(
            &mut connection,
            &mut graph,
            applied.as_ref(),
            &mut observation,
            &mut recovery,
            recovery_origin,
            meter_monitoring,
            &mut last_live_failure_reset,
        );
    }
    drop(graph);
    drop(connection);
    let _ = shutdown_complete.send(());
}

fn update_meter_monitoring(
    connection: &mut Option<PipewireConnection>,
    graph: &mut Option<LiveGraph>,
    applied: Option<&Config>,
    observation: &mut Option<EngineObservation>,
    enabled: bool,
) -> Result<(), EngineError> {
    let Some(config) = applied else {
        return Ok(());
    };
    let next = apply_native(
        connection,
        graph,
        Some(config),
        observation.as_ref(),
        config,
        enabled,
    )?;
    *observation = Some(next);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn service_native(
    connection: &mut Option<PipewireConnection>,
    graph: &mut Option<LiveGraph>,
    applied: Option<&Config>,
    observation: &mut Option<EngineObservation>,
    recovery: &mut RecoveryController,
    origin: Instant,
    meter_monitoring: bool,
    last_live_failure_reset: &mut Option<Instant>,
) {
    let now = Instant::now();
    let now_millis = elapsed_millis(origin);
    if let Some(current) = connection.as_ref() {
        let _ = current.dispatch_once(Duration::ZERO);
        if current.take_failure().is_some() {
            *graph = None;
            *connection = None;
            mark_degraded(observation, RecoveryFault::Core);
            recovery.fault(RecoveryFault::Core, now_millis);
        }
    }

    // Registry callbacks are already dispatched by the native owner loop.
    // Consume their debounced snapshot once and carry the cached descriptors
    // through Observe replies, keeping the 40 ms service monitor O(1) instead
    // of issuing a synchronous ~50 ms round trip on every tick.
    let registry_snapshot = connection
        .as_ref()
        .and_then(PipewireConnection::registry_snapshot_if_due);
    if let (Some(snapshot), Some(observation)) = (registry_snapshot.as_ref(), observation.as_mut())
    {
        observation.devices = input_descriptors(snapshot);
    }

    if let (Some(active_graph), Some(config)) = (graph.as_ref(), applied) {
        let graph_fault = active_graph.take_health_issue().map(|issue| match issue {
            GraphHealthIssue::CaptureStream | GraphHealthIssue::CaptureFormat => {
                RecoveryFault::CaptureStream
            }
            GraphHealthIssue::SourceStream | GraphHealthIssue::SourceFormat => {
                RecoveryFault::SourceStream
            }
        });
        let resolved_fault = graph_fault.or_else(|| {
            registry_snapshot.as_ref().and_then(|snapshot| {
                resolve_input(snapshot, config).map_or(
                    Some(RecoveryFault::InputUnavailable),
                    |node| {
                        (node != active_graph.target_node_name())
                            .then_some(RecoveryFault::DefaultChanged)
                    },
                )
            })
        });
        if let Some(fault) = resolved_fault {
            *graph = None;
            mark_degraded(observation, fault);
            recovery.fault(fault, now_millis);
        } else if active_graph.service_demand(now).is_err() {
            *graph = None;
            mark_degraded(observation, RecoveryFault::CaptureStream);
            recovery.fault(RecoveryFault::CaptureStream, now_millis);
        } else if live_failure_latched(active_graph.telemetry().snapshot().state)
            && live_failure_reset_due(*last_live_failure_reset, now)
        {
            // The live sink latches model and transport faults until its
            // documented deactivated reset runs. Without this poll a single
            // transient transport overflow or model error keeps the processed
            // output silent forever while consumers hold the source streaming,
            // because no demand edge or health fault ever fires. Advancing the
            // input generation rebuilds capture-side sink and model state
            // through the concurrency-safe atomic command; consumers drain the
            // superseded generation themselves.
            let _ = active_graph.capture().advance_input_generation();
            *last_live_failure_reset = Some(now);
        }
    }

    let Some(config) = applied.filter(|config| config.active || meter_monitoring) else {
        recovery.stop();
        return;
    };
    if graph.is_some() {
        return;
    }
    if recovery.poll(now_millis).is_none() {
        return;
    }
    if let Ok(recovered) = apply_native(
        connection,
        graph,
        None,
        observation.as_ref(),
        config,
        meter_monitoring,
    ) {
        *observation = Some(recovered);
        recovery.recovered();
    } else {
        if let Some(fault) = recovery.fault_kind() {
            mark_degraded(observation, fault);
        }
        recovery.failed(now_millis);
    }
}

/// Whether the live pipeline latched a failure that silences processed output.
#[must_use]
fn live_failure_latched(state: LiveState) -> bool {
    matches!(state, LiveState::ModelFailed | LiveState::TransportFailed)
}

/// Whether another latched-failure deactivated reset is due.
#[must_use]
fn live_failure_reset_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|previous| now.duration_since(previous) >= LIVE_FAILURE_RESET_INTERVAL)
}

fn resolve_input(snapshot: &RegistrySnapshot, config: &Config) -> Option<String> {
    let policy = SelectionPolicy {
        selector: (config.input.mode == InputMode::Selected).then(|| {
            noire_pipewire::DeviceSelector {
                node_name: config.input.stable_id.clone(),
                device_serial: None,
                device_name: None,
            }
        }),
        follow_default: config.input.mode == InputMode::FollowDefault,
        fallback_to_default: config.input.fallback_to_default,
    };
    match snapshot.resolve(&policy) {
        InputResolution::Selected(selected) => Some(selected.node_name),
        InputResolution::Unavailable(_) => None,
    }
}

fn mark_degraded(observation: &mut Option<EngineObservation>, fault: RecoveryFault) {
    let next = observation.get_or_insert_with(EngineObservation::default);
    next.state = LifecycleState::Degraded;
    next.metrics.resets = next.metrics.resets.saturating_add(1);
    next.fault = Some(recovery_error(fault));
}

fn degraded_observation(
    previous: Option<&EngineObservation>,
    error: Option<&EngineError>,
) -> EngineObservation {
    let mut observation = previous.cloned().unwrap_or_default();
    observation.state = LifecycleState::Degraded;
    observation.fault = error.cloned();
    observation
}

fn cache_observed_devices(
    observation: &mut Option<EngineObservation>,
    devices: &[InputDescriptor],
) {
    let current = observation.get_or_insert_with(|| EngineObservation {
        state: LifecycleState::Stopped,
        input_display_name: "System default".to_owned(),
        ..EngineObservation::default()
    });
    current.devices.clear();
    current.devices.extend_from_slice(devices);
}

fn recovery_error(fault: RecoveryFault) -> EngineError {
    match fault {
        RecoveryFault::Core => EngineError {
            code: "pipewire-unavailable",
            message: "the user PipeWire server disconnected".to_owned(),
            recovery: "Noire is retrying with bounded backoff; restart PipeWire if needed",
            retryable: true,
        },
        RecoveryFault::CaptureStream | RecoveryFault::SourceStream => EngineError {
            code: "audio-stream-failed",
            message: "a native audio stream stopped unexpectedly".to_owned(),
            recovery: "Noire is rebuilding the audio graph with bounded backoff",
            retryable: true,
        },
        RecoveryFault::InputUnavailable => EngineError {
            code: "input-unavailable",
            message: "the configured input is unavailable".to_owned(),
            recovery: "reconnect the selected input or choose another available input",
            retryable: true,
        },
        RecoveryFault::DefaultChanged => EngineError {
            code: "input-unavailable",
            message: "the session default input changed".to_owned(),
            recovery: "Noire is following the new default input",
            retryable: true,
        },
    }
}

fn classify_initial_fault(error: &EngineError) -> RecoveryFault {
    match error.code {
        "input-unavailable" => RecoveryFault::InputUnavailable,
        "audio-graph-unavailable" => RecoveryFault::CaptureStream,
        _ => RecoveryFault::Core,
    }
}

fn elapsed_millis(origin: Instant) -> u64 {
    u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[allow(clippy::too_many_lines)]
fn apply_native(
    connection: &mut Option<PipewireConnection>,
    graph: &mut Option<LiveGraph>,
    previous: Option<&Config>,
    previous_observation: Option<&EngineObservation>,
    config: &Config,
    meter_monitoring: bool,
) -> Result<EngineObservation, EngineError> {
    if !config.active && !meter_monitoring {
        *graph = None;
        return Ok(stopped_observation(
            connection,
            previous,
            previous_observation,
            config,
        ));
    }

    let requires_rebuild = graph.is_none()
        || previous.is_none_or(|previous| {
            previous.input != config.input || previous.output != config.output
        });
    let selected_label = if requires_rebuild {
        *graph = None;
        let connection = ensure_connection(connection)?;
        refresh_registry(connection);
        let snapshot = connection.registry_snapshot_now();
        let policy = SelectionPolicy {
            selector: (config.input.mode == InputMode::Selected).then(|| {
                noire_pipewire::DeviceSelector {
                    node_name: config.input.stable_id.clone(),
                    device_serial: None,
                    device_name: None,
                }
            }),
            follow_default: config.input.mode == InputMode::FollowDefault,
            fallback_to_default: config.input.fallback_to_default,
        };
        let selected = match snapshot.resolve(&policy) {
            InputResolution::Selected(selected) => selected,
            InputResolution::Unavailable(reason) => {
                return Err(EngineError {
                    code: "input-unavailable",
                    message: format!("the configured input could not be resolved: {reason:?}"),
                    recovery: "select an available input or enable explicit default fallback",
                    retryable: true,
                });
            }
        };
        let factory = FastEnhancerFactory::new().map_err(|error| EngineError {
            code: "model-initialization-failed",
            message: format!("the bundled FastEnhancer-B model could not initialize: {error}"),
            recovery: "restart Noire; reinstall if the condition persists",
            retryable: true,
        })?;
        let model = factory.create().map_err(|error| EngineError {
            code: "model-initialization-failed",
            message: format!("the bundled FastEnhancer-B model could not initialize: {error}"),
            recovery: "restart Noire; reinstall if the condition persists",
            retryable: true,
        })?;
        *graph = Some(
            LiveGraph::connect_with_latency(
                connection,
                &selected.node_name,
                model,
                match config.output.latency_profile {
                    LatencyProfile::Low => StreamLatency::Low,
                    LatencyProfile::Balanced => StreamLatency::Balanced,
                },
            )
            .map_err(|error| EngineError {
                code: "audio-graph-unavailable",
                message: format!("the live PipeWire graph could not start: {error}"),
                recovery: "verify PipeWire and the selected input, then retry",
                retryable: true,
            })?,
        );
        selected.label
    } else if config.input.mode == InputMode::Selected {
        previous_observation.map_or_else(
            || config.input.stable_id.clone(),
            |observation| observation.input_display_name.clone(),
        )
    } else {
        previous_observation.map_or_else(
            || "Default input".to_owned(),
            |observation| observation.input_display_name.clone(),
        )
    };

    let graph = graph.as_ref().ok_or_else(|| EngineError {
        code: "audio-graph-unavailable",
        message: "the live audio graph is absent".to_owned(),
        recovery: "retry the operation",
        retryable: true,
    })?;
    apply_controls(graph, config);
    graph
        .set_meter_monitoring(meter_monitoring)
        .map_err(|error| EngineError {
            code: "audio-meter-unavailable",
            message: format!("microphone metering could not change state: {error}"),
            recovery: "retry; restart PipeWire if the meter remains unavailable",
            retryable: true,
        })?;
    let mut next = observe_graph(
        graph,
        selected_label,
        connection
            .as_ref()
            .and_then(PipewireConnection::runtime_version)
            .unwrap_or_default(),
        config.active,
    );
    next.devices = if requires_rebuild {
        connection
            .as_ref()
            .map(|connection| input_descriptors(&connection.registry_snapshot_now()))
            .unwrap_or_default()
    } else {
        previous_observation
            .map(|observation| observation.devices.clone())
            .unwrap_or_default()
    };
    Ok(next)
}

fn stopped_observation(
    connection: &mut Option<PipewireConnection>,
    previous: Option<&Config>,
    previous_observation: Option<&EngineObservation>,
    config: &Config,
) -> EngineObservation {
    let input_unchanged = previous.is_some_and(|previous| previous.input == config.input);
    let previous_label = previous_observation
        .filter(|_| input_unchanged)
        .map(|observation| observation.input_display_name.as_str())
        .filter(|label| !label.is_empty());
    let mut devices = previous_observation
        .map(|observation| observation.devices.clone())
        .unwrap_or_default();
    let resolved_label = previous_label.map(ToOwned::to_owned).or_else(|| {
        let connection = ensure_connection(connection).ok()?;
        refresh_registry(connection);
        let snapshot = connection.registry_snapshot_now();
        devices = input_descriptors(&snapshot);
        resolve_input(&snapshot, config).and_then(|node_name| {
            snapshot
                .candidates()
                .iter()
                .find(|candidate| candidate.node_name == node_name)
                .map(|candidate| candidate.label.clone())
        })
    });
    EngineObservation {
        state: LifecycleState::Stopped,
        input_display_name: resolved_label.unwrap_or_else(|| {
            if config.input.mode == InputMode::Selected {
                config.input.stable_id.clone()
            } else {
                "System default".to_owned()
            }
        }),
        pipewire_version: connection
            .as_ref()
            .and_then(PipewireConnection::runtime_version)
            .or_else(|| {
                previous_observation.map(|observation| observation.pipewire_version.clone())
            })
            .unwrap_or_default(),
        devices,
        ..EngineObservation::default()
    }
}

fn ensure_connection(
    connection: &mut Option<PipewireConnection>,
) -> Result<&PipewireConnection, EngineError> {
    if connection.is_none() {
        *connection = Some(
            PipewireConnection::connect_default().map_err(|error| EngineError {
                code: "pipewire-unavailable",
                message: format!("could not connect to the user PipeWire server: {error}"),
                recovery: "start or repair the user PipeWire session, then retry",
                retryable: true,
            })?,
        );
    }
    connection.as_ref().ok_or_else(|| EngineError {
        code: "pipewire-unavailable",
        message: "the PipeWire connection is absent".to_owned(),
        recovery: "retry the operation",
        retryable: true,
    })
}

fn refresh_registry(connection: &PipewireConnection) {
    let _ = connection.request_roundtrip();
    for _ in 0..25 {
        let _ = connection.dispatch_once(Duration::from_millis(2));
    }
}

fn input_descriptors(snapshot: &RegistrySnapshot) -> Vec<InputDescriptor> {
    snapshot
        .candidates()
        .iter()
        .map(|node| InputDescriptor {
            stable_id: node.node_name.clone(),
            display_name: node.label.clone(),
            is_default: node.is_default,
            availability: match node.availability {
                DeviceAvailability::Available => "available",
                DeviceAvailability::Unavailable => "unavailable",
                DeviceAvailability::Unknown => "unknown",
            }
            .to_owned(),
        })
        .collect()
}

#[allow(clippy::cast_possible_truncation)]
fn apply_controls(graph: &LiveGraph, config: &Config) {
    let control = graph.control();
    control.set_enabled(config.suppression.enabled);
    control.set_strength(config.suppression.strength as f32);
    control.set_fail_mode(match config.suppression.fail_mode {
        ConfigFailMode::Closed => FailMode::Closed,
        ConfigFailMode::Open => FailMode::Open,
    });
}

fn observe_graph(
    graph: &LiveGraph,
    input_display_name: String,
    pipewire_version: String,
    processing_active: bool,
) -> EngineObservation {
    let live = graph.telemetry().snapshot();
    let capture = graph.capture().telemetry().snapshot();
    let source = graph.source().telemetry().snapshot();
    let fault = processing_active
        .then(|| live_state_error(live.state))
        .flatten();
    EngineObservation {
        state: if !processing_active {
            LifecycleState::Stopped
        } else if fault.is_none() {
            LifecycleState::Running
        } else {
            LifecycleState::Degraded
        },
        input_display_name,
        pipewire_version,
        metrics: Metrics {
            callback_p50_ns: live.callback_timing.percentile_ns(50),
            callback_p95_ns: live.callback_timing.percentile_ns(95),
            callback_p99_ns: live.callback_timing.percentile_ns(99),
            callback_max_ns: live.callback_timing.maximum_ns,
            model_p50_ns: live.model_timing.percentile_ns(50),
            model_p95_ns: live.model_timing.percentile_ns(95),
            model_p99_ns: live.model_timing.percentile_ns(99),
            model_max_ns: live.model_timing.maximum_ns,
            ring_current_samples: live.transport.current_frames,
            ring_high_water_samples: live.transport.high_water_frames,
            underflows: live.transport.underflows,
            overflows: live.transport.overflows,
            buffer_errors: capture
                .counters
                .malformed_chunks
                .saturating_add(source.malformed_buffers)
                .saturating_add(source.missing_buffers),
            model_errors: live.model_errors,
            resets: live
                .model_resets
                .saturating_add(live.transport.generation_resets),
            non_finite_samples: live
                .sanitized_samples
                .saturating_add(capture.counters.non_finite_samples),
            vad_probability: f64::from(live.vad_probability),
            rms: f64::from(live.rms),
            peak: f64::from(live.peak),
        },
        fault,
        devices: Vec::new(),
    }
}

fn live_state_error(state: LiveState) -> Option<EngineError> {
    match state {
        LiveState::Running | LiveState::DegradedPerformance => None,
        LiveState::ModelFailed => Some(EngineError {
            code: "model-processing-failed",
            message: "the suppression model failed while processing audio".to_owned(),
            recovery: "allow automatic recovery; restart Noire if the failure persists",
            retryable: true,
        }),
        LiveState::TransportFailed => Some(EngineError {
            code: "audio-transport-failed",
            message: "the processed audio transport stalled".to_owned(),
            recovery: "allow automatic recovery; restart Noire if silence persists",
            retryable: true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{atomic::Ordering, mpsc},
        time::{Duration, Instant},
    };

    use super::{
        LIVE_FAILURE_RESET_INTERVAL, LiveState, NativeAudioEngine, NativeCommand,
        cache_observed_devices, live_failure_latched, live_failure_reset_due, live_state_error,
    };
    use crate::{EngineObservation, LifecycleState};
    use noire_ipc::InputDescriptor;

    #[test]
    fn only_silence_latching_live_states_schedule_a_reset() {
        assert!(live_failure_latched(LiveState::ModelFailed));
        assert!(live_failure_latched(LiveState::TransportFailed));
        assert!(!live_failure_latched(LiveState::Running));
        // Degraded performance keeps producing audio, so it must never
        // trigger a deactivated reset.
        assert!(!live_failure_latched(LiveState::DegradedPerformance));
    }

    #[test]
    fn latched_failure_resets_are_rate_limited_but_prompt() {
        let now = Instant::now();
        assert!(live_failure_reset_due(None, now));

        assert!(
            !live_failure_reset_due(Some(now), now),
            "an immediate second reset would hot-loop on persistent failures"
        );
        let due = now + LIVE_FAILURE_RESET_INTERVAL;
        assert!(
            live_failure_reset_due(Some(now), due),
            "recovery after a transient stall must not wait longer than one interval"
        );
    }

    #[test]
    fn performance_degradation_does_not_publish_a_lifecycle_fault() {
        assert!(live_state_error(LiveState::DegradedPerformance).is_none());
        assert_eq!(
            live_state_error(LiveState::ModelFailed).map(|error| error.code),
            Some("model-processing-failed")
        );
        assert_eq!(
            live_state_error(LiveState::TransportFailed).map(|error| error.code),
            Some("audio-transport-failed")
        );
    }

    #[test]
    fn successful_input_query_seeds_an_empty_native_observation() {
        let mut observation: Option<EngineObservation> = None;
        let devices = vec![InputDescriptor {
            stable_id: "alsa_input.login-race".to_owned(),
            display_name: "Recovered microphone".to_owned(),
            is_default: true,
            availability: "available".to_owned(),
        }];

        cache_observed_devices(&mut observation, &devices);

        let observation = observation.unwrap_or_default();
        assert_eq!(observation.state, LifecycleState::Stopped);
        assert_eq!(observation.devices, devices);
        assert_eq!(observation.input_display_name, "System default");
    }

    #[test]
    fn queued_command_is_cancelled_when_its_caller_times_out() {
        let (commands, receiver) = mpsc::sync_channel(1);
        let (shutdown_complete, shutdown_wait) = mpsc::sync_channel(1);
        let mut engine = NativeAudioEngine {
            commands: Some(commands),
            shutdown_complete: shutdown_wait,
            thread: None,
        };

        let result = engine.request_with_timeout(
            |reply, cancelled| NativeCommand::Observe { cancelled, reply },
            Duration::from_millis(1),
        );
        assert_eq!(
            result.as_ref().err().map(|error| error.code),
            Some("audio-command-timeout")
        );
        let cancelled = match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(NativeCommand::Observe { cancelled, .. }) => Some(cancelled),
            _ => None,
        };
        assert!(
            cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)),
            "expected a cancelled queued observe command"
        );

        engine.commands.take();
        let _ = shutdown_complete.send(());
    }
}
