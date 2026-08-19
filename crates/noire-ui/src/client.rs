//! Background D-Bus worker isolated from the GTK main thread.

use std::{
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    time::Duration,
};

use futures_util::StreamExt;
use noire_ipc::{DiagnosticReport, InputDescriptor, Metrics, Noire1Proxy, Snapshot};

use crate::state::UserError;

#[derive(Clone, Debug)]
pub(crate) enum Request {
    Refresh,
    SetActive(bool),
    SelectInput(String),
    SetSuppressionEnabled(bool),
    SetStrength(f64),
    SetLatencyProfile(String),
    SetFailMode(String),
    SetLaunchAtLogin(bool),
    Retry,
    Diagnostics,
    Shutdown,
}

#[derive(Debug)]
pub(crate) enum Response {
    State {
        snapshot: Snapshot,
        inputs: Vec<InputDescriptor>,
        refresh: bool,
        request_complete: bool,
    },
    Rejected {
        error: UserError,
        recovered: Option<(Snapshot, Vec<InputDescriptor>)>,
        request_complete: bool,
    },
    Diagnostics(DiagnosticReport),
    Meters(Metrics),
}

pub(crate) struct WorkerChannels {
    pub requests: tokio::sync::mpsc::Sender<Request>,
    pub responses: Receiver<Response>,
}

pub(crate) fn spawn() -> WorkerChannels {
    let (request_sender, request_receiver) = tokio::sync::mpsc::channel(8);
    let (response_sender, response_receiver) = mpsc::sync_channel(8);
    let startup_errors = response_sender.clone();
    let spawn_result = std::thread::Builder::new()
        .name("noire-dbus".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(_error) => {
                    let _ignored = response_sender.send(Response::Rejected {
                        error: communication_error(
                            "ui-runtime-unavailable",
                            "Noire could not initialize background communication.",
                            "Restart Noire; reinstall it if the condition persists.",
                        ),
                        recovered: None,
                        request_complete: false,
                    });
                    return;
                }
            };
            runtime.block_on(worker_loop(request_receiver, response_sender));
        });
    if let Err(_error) = spawn_result {
        let _ignored = startup_errors.send(Response::Rejected {
            error: communication_error(
                "ui-thread-unavailable",
                "Noire could not start its background communication thread.",
                "Free process resources and restart Noire.",
            ),
            recovered: None,
            request_complete: false,
        });
    }

    WorkerChannels {
        requests: request_sender,
        responses: response_receiver,
    }
}

const INITIAL_RECONNECT_BACKOFF: Duration = Duration::from_millis(250);
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(4);

async fn worker_loop(
    mut requests: tokio::sync::mpsc::Receiver<Request>,
    responses: SyncSender<Response>,
) {
    let Some(mut request) = requests.recv().await else {
        return;
    };
    let mut request_complete = true;
    let mut backoff = INITIAL_RECONNECT_BACKOFF;
    loop {
        if matches!(request, Request::Shutdown) {
            return;
        }
        let connection = match zbus::Connection::session().await {
            Ok(connection) => connection,
            Err(_error) => {
                if responses
                    .send(rejected(
                        communication_error(
                            "session-bus-unavailable",
                            "The desktop session bus is unavailable.",
                            "Log in again, then retry.",
                        ),
                        request_complete,
                    ))
                    .is_err()
                {
                    return;
                }
                let Some(next) = next_reconnect(&mut requests, backoff).await else {
                    return;
                };
                (request, request_complete) = next;
                backoff = next_backoff(backoff);
                continue;
            }
        };
        let mut established = false;
        if serve_connection(
            &connection,
            request,
            request_complete,
            &mut requests,
            &responses,
            &mut established,
        )
        .await
        {
            return;
        }
        if established {
            backoff = INITIAL_RECONNECT_BACKOFF;
        }
        let Some(next) = next_reconnect(&mut requests, backoff).await else {
            return;
        };
        (request, request_complete) = next;
        backoff = next_backoff(backoff);
    }
}

async fn next_reconnect(
    requests: &mut tokio::sync::mpsc::Receiver<Request>,
    delay: Duration,
) -> Option<(Request, bool)> {
    tokio::select! {
        request = requests.recv() => request.map(|request| (request, true)),
        () = tokio::time::sleep(delay) => Some((Request::Refresh, false)),
    }
}

fn next_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(MAX_RECONNECT_BACKOFF)
}

// Keeping one select loop makes command completion, signal convergence, owner
// loss, and shutdown ordering directly auditable.
#[allow(clippy::too_many_lines)]
async fn serve_connection(
    connection: &zbus::Connection,
    initial_request: Request,
    initial_request_complete: bool,
    requests: &mut tokio::sync::mpsc::Receiver<Request>,
    responses: &SyncSender<Response>,
    established: &mut bool,
) -> bool {
    let proxy = match Noire1Proxy::new(connection).await {
        Ok(proxy) => proxy,
        Err(_error) => {
            let _ignored = responses.send(rejected(daemon_unavailable(), initial_request_complete));
            return false;
        }
    };
    let Ok(mut state_changes) = proxy.receive_state_changed().await else {
        let _ignored = responses.send(rejected(daemon_unavailable(), initial_request_complete));
        return false;
    };
    let Ok(mut device_changes) = proxy.receive_devices_changed().await else {
        let _ignored = responses.send(rejected(daemon_unavailable(), initial_request_complete));
        return false;
    };
    let Ok(mut meter_changes) = proxy.receive_meters_changed().await else {
        let _ignored = responses.send(rejected(daemon_unavailable(), initial_request_complete));
        return false;
    };
    let Ok(mut owner_changes) = proxy.inner().receive_owner_changed().await else {
        let _ignored = responses.send(rejected(daemon_unavailable(), initial_request_complete));
        return false;
    };
    if proxy.subscribe_meters().await.is_err() {
        let _ignored = responses.send(rejected(daemon_unavailable(), initial_request_complete));
        return false;
    }

    let response = execute(&proxy, initial_request).await;
    let disconnected = matches!(
        response,
        Response::Rejected {
            recovered: None,
            ..
        }
    );
    if responses
        .send(with_completion(response, initial_request_complete))
        .is_err()
    {
        return true;
    }
    if disconnected {
        return false;
    }
    *established = true;

    loop {
        tokio::select! {
            request = requests.recv() => {
                let Some(request) = request else {
                    let _ignored = proxy.unsubscribe_meters().await;
                    return true;
                };
                if matches!(request, Request::Shutdown) {
                    let _ignored = proxy.unsubscribe_meters().await;
                    return true;
                }
                let response = execute(&proxy, request).await;
                let disconnected = matches!(response, Response::Rejected { recovered: None, .. });
                if responses.send(with_completion(response, true)).is_err() {
                    return true;
                }
                if disconnected {
                    return false;
                }
            }
            signal = state_changes.next() => {
                if signal.is_none() {
                    return false;
                }
                if responses.send(refresh(&proxy).await).is_err() {
                    return true;
                }
            }
            signal = device_changes.next() => {
                if signal.is_none() {
                    return false;
                }
                if responses.send(refresh(&proxy).await).is_err() {
                    return true;
                }
            }
            signal = meter_changes.next() => {
                let Some(signal) = signal else {
                    return false;
                };
                if let Ok(arguments) = signal.args()
                    && !send_meter(responses, Response::Meters(arguments.metrics().clone())) {
                    return true;
                }
            }
            owner = owner_changes.next() => {
                if let Some(Some(_new_owner)) = owner {
                    if responses.send(refresh(&proxy).await).is_err() {
                        return true;
                    }
                } else {
                    let _ignored = responses.send(rejected(daemon_unavailable(), false));
                    return false;
                }
            }
        }
    }
}

fn send_meter(responses: &SyncSender<Response>, response: Response) -> bool {
    match responses.try_send(response) {
        Ok(()) | Err(TrySendError::Full(_)) => true,
        Err(TrySendError::Disconnected(_)) => false,
    }
}

fn with_completion(response: Response, request_complete: bool) -> Response {
    match response {
        Response::State {
            snapshot,
            inputs,
            refresh,
            ..
        } => Response::State {
            snapshot,
            inputs,
            refresh,
            request_complete,
        },
        Response::Rejected {
            error, recovered, ..
        } => Response::Rejected {
            error,
            recovered,
            request_complete,
        },
        other => other,
    }
}

async fn refresh(proxy: &Noire1Proxy<'_>) -> Response {
    with_completion(execute(proxy, Request::Refresh).await, false)
}

async fn execute(proxy: &Noire1Proxy<'_>, request: Request) -> Response {
    let is_refresh = matches!(request, Request::Refresh);
    if matches!(request, Request::Diagnostics) {
        return match proxy.diagnostics().await {
            Ok(report) => Response::Diagnostics(report),
            Err(_error) => Response::Rejected {
                error: communication_error(
                    "diagnostics-unavailable",
                    "Noire could not read the diagnostic report.",
                    "Retry; restart the Noire user service if the problem persists.",
                ),
                recovered: recover(proxy).await,
                request_complete: false,
            },
        };
    }
    let result = match request {
        Request::Refresh => proxy.get_snapshot().await,
        mutation => {
            let revision = match proxy.get_snapshot().await {
                Ok(snapshot) => snapshot.revision,
                Err(_error) => {
                    return rejected(
                        communication_error(
                            "daemon-state-unavailable",
                            "Noire could not read current daemon state.",
                            "Start or restart the Noire user service, then retry.",
                        ),
                        false,
                    );
                }
            };
            match mutation {
                Request::SetActive(true) => proxy.start(revision).await,
                Request::SetActive(false) => proxy.stop(revision).await,
                Request::SelectInput(stable_id) => proxy.select_input(&stable_id, revision).await,
                Request::SetSuppressionEnabled(enabled) => {
                    proxy.set_suppression_enabled(enabled, revision).await
                }
                Request::SetStrength(strength) => proxy.set_strength(strength, revision).await,
                Request::SetLatencyProfile(profile) => {
                    proxy.set_latency_profile(&profile, revision).await
                }
                Request::SetFailMode(mode) => proxy.set_fail_mode(&mode, revision).await,
                Request::SetLaunchAtLogin(enabled) => {
                    proxy.set_launch_at_login(enabled, revision).await
                }
                Request::Retry => proxy.retry(revision).await,
                Request::Refresh | Request::Diagnostics | Request::Shutdown => {
                    return rejected(
                        UserError::new(
                            "ui-invalid-request",
                            "Noire created an invalid internal request.",
                            "Restart Noire; report the problem if it happens again.",
                            false,
                        ),
                        false,
                    );
                }
            }
        }
    };

    match result {
        Ok(snapshot) => match proxy.list_inputs().await {
            Ok(inputs) => Response::State {
                snapshot,
                inputs,
                refresh: is_refresh,
                request_complete: false,
            },
            Err(_error) => Response::Rejected {
                error: communication_error(
                    "input-list-unavailable",
                    "Daemon state loaded, but microphones could not be listed.",
                    "Retry; restart PipeWire if no microphones appear.",
                ),
                recovered: Some((snapshot, Vec::new())),
                request_complete: false,
            },
        },
        Err(error) => {
            let recovered = recover(proxy).await;
            Response::Rejected {
                error: if is_refresh && recovered.is_none() {
                    daemon_unavailable()
                } else {
                    mutation_error(&error)
                },
                recovered,
                request_complete: false,
            }
        }
    }
}

async fn recover(proxy: &Noire1Proxy<'_>) -> Option<(Snapshot, Vec<InputDescriptor>)> {
    let snapshot = proxy.get_snapshot().await.ok()?;
    let inputs = proxy.list_inputs().await.ok()?;
    Some((snapshot, inputs))
}

fn rejected(error: UserError, request_complete: bool) -> Response {
    Response::Rejected {
        error,
        recovered: None,
        request_complete,
    }
}

fn mutation_error(error: &zbus::Error) -> UserError {
    let technical = error.to_string();
    mutation_error_copy(&technical)
}

fn mutation_error_copy(technical: &str) -> UserError {
    if technical.contains(".Conflict") {
        UserError::new(
            "conflict",
            "Another control changed Noire first.",
            "Current daemon settings were restored; retry your change.",
            true,
        )
    } else if technical.contains(".InvalidArgument") {
        UserError::new(
            "invalid-argument",
            "That setting was not valid.",
            "Correct the value and retry; current daemon settings were restored.",
            false,
        )
    } else if technical.contains(".Persistence") {
        UserError::new(
            "config-persistence",
            "Noire could not save the setting.",
            "Check configuration storage, permissions, and free space, then retry.",
            true,
        )
    } else if technical.contains(".LaunchManager") {
        UserError::new(
            "launch-manager-unavailable",
            "The user service manager could not change launch-at-login.",
            "Check the user systemd session, then retry.",
            true,
        )
    } else if technical.contains(".Busy") {
        UserError::new(
            "audio-command-busy",
            "The audio engine is busy.",
            "Wait briefly, then retry.",
            true,
        )
    } else if technical.contains(".Unavailable") {
        UserError::new(
            "audio-unavailable",
            "Audio processing is unavailable.",
            "Check the microphone and PipeWire session, then retry.",
            true,
        )
    } else {
        UserError::new(
            "daemon-request-failed",
            "Noire could not apply that change.",
            "Current daemon settings were restored; restart Noire if it happens again.",
            false,
        )
    }
}

fn communication_error(code: &str, cause: &str, recovery: &str) -> UserError {
    UserError::new(code, cause, recovery, true)
}

fn daemon_unavailable() -> UserError {
    communication_error(
        "daemon-unavailable",
        "The Noire background service is unavailable.",
        "Start or restart the Noire user service, then retry.",
    )
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        fs,
        future::{self, Future},
        path::PathBuf,
        pin::Pin,
        sync::Arc,
        time::{Duration, SystemTime},
    };

    use noire_config::{Config, ConfigStore};
    use noire_ipc::{InputDescriptor, Noire1Proxy, Snapshot};
    use noired::{
        AudioEngine, Daemon, EngineError, EngineObservation, LaunchManager, LaunchManagerError,
        LifecycleState, NoireService, claim_name, register_service,
    };

    use super::{
        INITIAL_RECONNECT_BACKOFF, MAX_RECONNECT_BACKOFF, Request, Response, mutation_error_copy,
        next_backoff, spawn,
    };

    #[derive(Default)]
    struct FakeEngine;

    impl AudioEngine for FakeEngine {
        fn apply(&mut self, config: &Config) -> Result<EngineObservation, EngineError> {
            if config.active {
                return Err(EngineError {
                    code: "test-audio-unavailable",
                    message: "the test audio engine rejected start".to_owned(),
                    recovery: "leave processing stopped during this test",
                    retryable: true,
                });
            }
            Ok(EngineObservation {
                state: LifecycleState::Stopped,
                input_display_name: "Session Test Microphone".to_owned(),
                pipewire_version: "test-1.0".to_owned(),
                ..EngineObservation::default()
            })
        }

        fn inputs(&mut self) -> Result<Vec<InputDescriptor>, EngineError> {
            Ok(vec![InputDescriptor {
                stable_id: "alsa_input.session-test".to_owned(),
                display_name: "Session Test Microphone".to_owned(),
                is_default: true,
                availability: "available".to_owned(),
            }])
        }
    }

    #[derive(Default)]
    struct FakeLaunchManager;

    impl LaunchManager for FakeLaunchManager {
        fn set_enabled<'a>(
            &'a self,
            _enabled: bool,
        ) -> Pin<Box<dyn Future<Output = Result<(), LaunchManagerError>> + Send + 'a>> {
            Box::pin(future::ready(Ok(())))
        }
    }

    fn temporary_store() -> Result<(PathBuf, ConfigStore), Box<dyn Error>> {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "noire-phase8-session-{}-{nonce}",
            std::process::id()
        ));
        Ok((
            root.clone(),
            ConfigStore::new(root.join("noire/config.toml")),
        ))
    }

    async fn start_server(store: ConfigStore) -> Result<zbus::Connection, Box<dyn Error>> {
        let server = zbus::Connection::session().await?;
        claim_name(&server).await?;
        let loaded = store.load()?;
        let daemon = Daemon::new(store, Box::new(FakeEngine), loaded);
        register_service(
            &server,
            NoireService::new(daemon, Arc::new(FakeLaunchManager)),
        )
        .await?;
        Ok(server)
    }

    fn receive(
        runtime: &tokio::runtime::Runtime,
        channels: &super::WorkerChannels,
    ) -> Result<Response, Box<dyn Error>> {
        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(50)).await });
        Ok(channels.responses.recv_timeout(Duration::from_secs(2))?)
    }

    fn receive_completed(
        runtime: &tokio::runtime::Runtime,
        channels: &super::WorkerChannels,
    ) -> Result<Response, Box<dyn Error>> {
        loop {
            let response = receive(runtime, channels)?;
            match &response {
                Response::State {
                    request_complete: true,
                    ..
                }
                | Response::Rejected {
                    request_complete: true,
                    ..
                }
                | Response::Diagnostics(_) => return Ok(response),
                Response::State { .. } | Response::Rejected { .. } | Response::Meters(_) => {}
            }
        }
    }

    fn receive_external_revision(
        runtime: &tokio::runtime::Runtime,
        channels: &super::WorkerChannels,
        revision: u64,
    ) -> Result<Snapshot, Box<dyn Error>> {
        loop {
            if let Response::State {
                snapshot,
                request_complete: false,
                ..
            } = receive(runtime, channels)?
                && snapshot.revision >= revision
            {
                return Ok(snapshot);
            }
        }
    }

    fn state(response: Response) -> Result<Snapshot, Box<dyn Error>> {
        match response {
            Response::State { snapshot, .. } => Ok(snapshot),
            Response::Rejected { error, .. } => {
                Err(format!("expected state response, got rejection: {}", error.cause).into())
            }
            Response::Diagnostics(_) => Err("expected state response, got diagnostics".into()),
            Response::Meters(_) => Err("expected state response, got meters".into()),
        }
    }

    fn assert_strength(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < f64::EPSILON);
    }

    #[test]
    fn transport_errors_map_to_plain_actionable_copy() {
        let unavailable = mutation_error_copy(
            "org.freedesktop.DBus.Error: io.github.rayan6ms.Noire.Noire1.Error.Unavailable",
        );
        assert!(unavailable.recovery.contains("PipeWire"));
        assert!(!unavailable.cause.contains("org.freedesktop"));
        let conflict = mutation_error_copy("service.Error.Conflict");
        assert_eq!(conflict.code, "conflict");
        assert!(conflict.cause.contains("changed Noire"));
        assert!(
            mutation_error_copy("service.Error.Busy")
                .recovery
                .contains("retry")
        );
    }

    #[test]
    fn reconnect_backoff_is_exponential_and_capped() {
        let mut delay = INITIAL_RECONNECT_BACKOFF;
        let mut observed = Vec::new();
        for _ in 0..8 {
            observed.push(delay);
            delay = next_backoff(delay);
        }
        assert_eq!(observed[0], Duration::from_millis(250));
        assert_eq!(observed[1], Duration::from_millis(500));
        assert_eq!(observed[4], MAX_RECONNECT_BACKOFF);
        assert_eq!(observed[7], MAX_RECONNECT_BACKOFF);
    }

    #[test]
    #[ignore = "requires a private dbus-run-session"]
    fn dbus_worker_converges_after_external_change_rejection_and_restart()
    -> Result<(), Box<dyn Error>> {
        let (root, store) = temporary_store()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let server = runtime.block_on(start_server(store.clone()))?;
        let channels = spawn();

        channels.requests.blocking_send(Request::Refresh)?;
        let initial = state(receive_completed(&runtime, &channels)?)?;
        assert_eq!(initial.revision, 1);
        assert_strength(initial.strength, 0.55);

        channels.requests.blocking_send(Request::Diagnostics)?;
        match receive_completed(&runtime, &channels)? {
            Response::Diagnostics(report) => {
                assert_eq!(report.schema_version, 1);
                assert!(report.privacy.contains("no audio"));
                assert!(report.journal_hint.contains("noire.service"));
            }
            response => return Err(format!("expected diagnostic report: {response:?}").into()),
        }

        channels
            .requests
            .blocking_send(Request::SetStrength(0.42))?;
        let changed = state(receive_completed(&runtime, &channels)?)?;
        assert_strength(changed.strength, 0.42);
        assert!(changed.revision > initial.revision);

        let external = runtime.block_on(zbus::Connection::session())?;
        let externally_changed = runtime.block_on(async {
            let proxy = Noire1Proxy::new(&external).await?;
            proxy.set_strength(0.73, changed.revision).await
        })?;
        let refreshed =
            receive_external_revision(&runtime, &channels, externally_changed.revision)?;
        assert_eq!(refreshed.revision, externally_changed.revision);
        assert_strength(refreshed.strength, 0.73);

        channels.requests.blocking_send(Request::SetActive(true))?;
        match receive_completed(&runtime, &channels)? {
            Response::Rejected {
                error,
                recovered: Some((authoritative, _)),
                ..
            } => {
                assert_eq!(error.code, "audio-unavailable");
                assert!(error.cause.contains("unavailable"));
                assert!(!error.recovery.is_empty());
                assert!(!authoritative.active);
                assert_strength(authoritative.strength, 0.73);
            }
            response => return Err(format!("unexpected rejection response: {response:?}").into()),
        }

        drop(external);
        drop(server);
        std::thread::sleep(Duration::from_millis(50));
        channels.requests.blocking_send(Request::Refresh)?;
        match receive_completed(&runtime, &channels)? {
            Response::Rejected {
                recovered: None, ..
            } => {}
            response => {
                return Err(format!("daemon disappearance did not reject: {response:?}").into());
            }
        }

        let replacement = runtime.block_on(start_server(store))?;
        channels.requests.blocking_send(Request::Refresh)?;
        let restored = state(receive_completed(&runtime, &channels)?)?;
        assert_strength(restored.strength, 0.73);
        assert!(!restored.active);

        channels.requests.blocking_send(Request::Shutdown)?;
        drop(replacement);
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
