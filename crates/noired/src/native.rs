//! Bounded command adapter owning all `PipeWire` objects on one native thread.

use std::{
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use noire_config::{Config, FailMode as ConfigFailMode, InputMode};
use noire_ipc::{InputDescriptor, Metrics};
use noire_model::DenoiserFactory;
use noire_model_rnnoise::RnnoiseFactory;
use noire_pipewire::{
    DeviceAvailability, FailMode, GraphHealthIssue, InputResolution, LiveGraph, LiveState,
    PipewireConnection, RegistrySnapshot, SelectionPolicy,
};

use crate::{
    AudioEngine, EngineError, EngineObservation, LifecycleState, RecoveryController, RecoveryFault,
};

const COMMAND_CAPACITY: usize = 16;
const COMMAND_TIMEOUT: Duration = Duration::from_millis(450);
const DISPATCH_QUANTUM: Duration = Duration::from_millis(2);

enum NativeCommand {
    Apply {
        config: Config,
        reply: SyncSender<Result<EngineObservation, EngineError>>,
    },
    Inputs {
        reply: SyncSender<Result<Vec<InputDescriptor>, EngineError>>,
    },
    Observe {
        reply: SyncSender<Result<EngineObservation, EngineError>>,
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
        build: impl FnOnce(SyncSender<Result<T, EngineError>>) -> NativeCommand,
    ) -> Result<T, EngineError> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.commands
            .as_ref()
            .ok_or_else(stopped_error)?
            .try_send(build(reply))
            .map_err(command_error)?;
        receive
            .recv_timeout(COMMAND_TIMEOUT)
            .map_err(|_| EngineError {
                code: "audio-command-timeout",
                message: "the audio control thread did not respond within 450 ms".to_owned(),
                recovery: "retry; restart Noire if the condition persists",
                retryable: true,
            })?
    }
}

impl AudioEngine for NativeAudioEngine {
    fn apply(&mut self, config: &Config) -> Result<EngineObservation, EngineError> {
        self.request(|reply| NativeCommand::Apply {
            config: config.clone(),
            reply,
        })
    }

    fn inputs(&mut self) -> Result<Vec<InputDescriptor>, EngineError> {
        self.request(|reply| NativeCommand::Inputs { reply })
    }

    fn observe(&mut self, _previous: &EngineObservation) -> Result<EngineObservation, EngineError> {
        self.request(|reply| NativeCommand::Observe { reply })
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

fn run_native(receiver: &Receiver<NativeCommand>, shutdown_complete: &SyncSender<()>) {
    let mut connection: Option<PipewireConnection> = None;
    let mut graph: Option<LiveGraph> = None;
    let mut applied: Option<Config> = None;
    let mut observation: Option<EngineObservation> = None;
    let recovery_origin = Instant::now();
    let mut recovery = RecoveryController::default();
    loop {
        match receiver.recv_timeout(DISPATCH_QUANTUM) {
            Ok(NativeCommand::Apply { config, reply }) => {
                let previous_config = applied.clone();
                let previous_observation = observation.clone();
                let result = apply_native(
                    &mut connection,
                    &mut graph,
                    previous_config.as_ref(),
                    previous_observation.as_ref(),
                    &config,
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
            Ok(NativeCommand::Inputs { reply }) => {
                let result = ensure_connection(&mut connection).map(|connection| {
                    refresh_registry(connection);
                    input_descriptors(connection)
                });
                let _ = reply.send(result);
            }
            Ok(NativeCommand::Observe { reply }) => {
                if let (Some(connection), Some(graph), Some(previous)) =
                    (connection.as_ref(), graph.as_ref(), observation.as_ref())
                {
                    observation = Some(observe_graph(
                        graph,
                        previous.input_display_name.clone(),
                        connection.runtime_version().unwrap_or_default(),
                    ));
                }
                let _ = reply.send(Ok(observation.clone().unwrap_or_default()));
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
        );
    }
    drop(graph);
    drop(connection);
    let _ = shutdown_complete.send(());
}

fn service_native(
    connection: &mut Option<PipewireConnection>,
    graph: &mut Option<LiveGraph>,
    applied: Option<&Config>,
    observation: &mut Option<EngineObservation>,
    recovery: &mut RecoveryController,
    origin: Instant,
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

    if let (Some(current), Some(active_graph), Some(config)) =
        (connection.as_ref(), graph.as_ref(), applied)
    {
        let graph_fault = active_graph.take_health_issue().map(|issue| match issue {
            GraphHealthIssue::CaptureStream | GraphHealthIssue::CaptureFormat => {
                RecoveryFault::CaptureStream
            }
            GraphHealthIssue::SourceStream | GraphHealthIssue::SourceFormat => {
                RecoveryFault::SourceStream
            }
        });
        let snapshot = current.registry_snapshot_if_due();
        let resolved_fault = graph_fault.or_else(|| {
            snapshot.as_ref().and_then(|snapshot| {
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
        }
    }

    let Some(config) = applied.filter(|config| config.active) else {
        recovery.stop();
        return;
    };
    if graph.is_some() {
        return;
    }
    if recovery.poll(now_millis).is_none() {
        return;
    }
    if let Ok(recovered) = apply_native(connection, graph, None, observation.as_ref(), config) {
        *observation = Some(recovered);
        recovery.recovered();
    } else {
        if let Some(fault) = recovery.fault_kind() {
            mark_degraded(observation, fault);
        }
        recovery.failed(now_millis);
    }
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

fn apply_native(
    connection: &mut Option<PipewireConnection>,
    graph: &mut Option<LiveGraph>,
    previous: Option<&Config>,
    previous_observation: Option<&EngineObservation>,
    config: &Config,
) -> Result<EngineObservation, EngineError> {
    if !config.active {
        *graph = None;
        return Ok(EngineObservation::default());
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
        let factory = RnnoiseFactory::new().map_err(|error| EngineError {
            code: "model-initialization-failed",
            message: format!("the bundled RNNoise model could not initialize: {error}"),
            recovery: "restart Noire; reinstall if the condition persists",
            retryable: true,
        })?;
        let model = factory.create().map_err(|error| EngineError {
            code: "model-initialization-failed",
            message: format!("the bundled RNNoise model could not initialize: {error}"),
            recovery: "restart Noire; reinstall if the condition persists",
            retryable: true,
        })?;
        *graph = Some(
            LiveGraph::connect(connection, &selected.node_name, model).map_err(|error| {
                EngineError {
                    code: "audio-graph-unavailable",
                    message: format!("the live PipeWire graph could not start: {error}"),
                    recovery: "verify PipeWire and the selected input, then retry",
                    retryable: true,
                }
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
    Ok(observe_graph(
        graph,
        selected_label,
        connection
            .as_ref()
            .and_then(PipewireConnection::runtime_version)
            .unwrap_or_default(),
    ))
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

fn input_descriptors(connection: &PipewireConnection) -> Vec<InputDescriptor> {
    connection
        .registry_snapshot_now()
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
) -> EngineObservation {
    let live = graph.telemetry().snapshot();
    let capture = graph.capture().telemetry().snapshot();
    let source = graph.source().telemetry().snapshot();
    EngineObservation {
        state: if live.state == LiveState::Running {
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
        fault: None,
    }
}
