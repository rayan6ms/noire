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
    DeviceAvailability, FailMode, InputResolution, LiveGraph, LiveState, PipewireConnection,
    SelectionPolicy,
};

use crate::{AudioEngine, EngineError, EngineObservation, LifecycleState};

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
    Shutdown,
}

/// Fixed-capacity daemon-to-PipeWire command endpoint.
pub struct NativeAudioEngine {
    commands: SyncSender<NativeCommand>,
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
        let thread = thread::Builder::new()
            .name("noire-pipewire".to_owned())
            .spawn(move || run_native(&receiver))
            .map_err(|error| EngineError {
                code: "audio-thread-unavailable",
                message: format!("could not create the audio control thread: {error}"),
                recovery: "free process resources and restart Noire",
                retryable: true,
            })?;
        Ok(Self {
            commands,
            thread: Some(thread),
        })
    }

    fn request<T>(
        &self,
        build: impl FnOnce(SyncSender<Result<T, EngineError>>) -> NativeCommand,
    ) -> Result<T, EngineError> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.commands
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
}

impl Drop for NativeAudioEngine {
    fn drop(&mut self) {
        let _ = self.commands.try_send(NativeCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
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

fn run_native(receiver: &Receiver<NativeCommand>) {
    let mut connection: Option<PipewireConnection> = None;
    let mut graph: Option<LiveGraph> = None;
    let mut applied: Option<Config> = None;
    let mut observation: Option<EngineObservation> = None;
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
            Ok(NativeCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(connection) = connection.as_ref() {
            let _ = connection.dispatch_once(Duration::ZERO);
            if let Some(graph) = graph.as_ref() {
                let _ = graph.service_demand(Instant::now());
            }
        }
    }
    drop(graph);
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
    }
}
