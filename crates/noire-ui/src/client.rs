//! Background D-Bus worker isolated from the GPUI main thread.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    time::Duration,
};

use futures_util::StreamExt;
use noire_ipc::{DiagnosticReport, InputDescriptor, Metrics, Noire1Proxy, Snapshot};

use crate::{state::UserError, tray::TrayRuntime};

#[derive(Clone, Debug)]
pub(crate) enum Request {
    Refresh,
    SetActive(bool),
    SetStartWithNoiseReduction(bool),
    SelectInput(String),
    SetSuppressionEnabled(bool),
    SetStrength(f64),
    SetLatencyProfile(String),
    SetFailMode(String),
    Retry,
    Diagnostics,
    Shutdown,
}

#[derive(Debug)]
pub(crate) enum Response {
    State {
        snapshot: Snapshot,
        inputs: Vec<InputDescriptor>,
        start_with_noise_reduction: bool,
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

pub(crate) fn spawn(subscribe_to_meters: bool) -> WorkerChannels {
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
            runtime.block_on(worker_loop(
                request_receiver,
                response_sender,
                subscribe_to_meters,
            ));
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

enum TrayControlCommand {
    Toggle,
    StopAndShutdown(SyncSender<bool>),
}

enum PendingTrayAction {
    Startup(SyncSender<bool>),
    Toggle,
    Stop {
        reply: SyncSender<bool>,
        attempts: u8,
    },
}

/// Keeps tray state synchronized even while no GPUI window exists.
#[derive(Clone)]
pub(crate) struct TrayController {
    commands: SyncSender<TrayControlCommand>,
    busy: Arc<AtomicBool>,
    tray: TrayRuntime,
}

impl TrayController {
    pub(crate) fn start(tray: TrayRuntime) -> (Self, Receiver<bool>) {
        let (commands, command_receiver) = mpsc::sync_channel(4);
        let (initialized, initialization) = mpsc::sync_channel(1);
        let busy = Arc::new(AtomicBool::new(true));
        tray.set_busy(true);
        let worker_tray = tray.clone();
        let worker_busy = Arc::clone(&busy);
        let spawn_result = std::thread::Builder::new()
            .name("noire-tray-control".to_owned())
            .spawn(move || {
                tray_control_loop(&worker_tray, initialized, &command_receiver, &worker_busy);
                worker_busy.store(false, Ordering::Release);
                worker_tray.set_busy(false);
            });
        if spawn_result.is_err() {
            busy.store(false, Ordering::Release);
            tray.set_busy(false);
        }
        (
            Self {
                commands,
                busy,
                tray,
            },
            initialization,
        )
    }

    pub(crate) fn toggle(&self) {
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        self.tray.set_busy(true);
        if self.commands.try_send(TrayControlCommand::Toggle).is_err() {
            self.busy.store(false, Ordering::Release);
            self.tray.set_busy(false);
        }
    }

    pub(crate) fn begin_external_change(&self) -> bool {
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        self.tray.set_busy(true);
        true
    }

    pub(crate) fn finish_external_change(&self) {
        self.busy.store(false, Ordering::Release);
        self.tray.set_busy(false);
    }

    #[must_use]
    pub(crate) fn busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }

    #[must_use]
    pub(crate) fn stop_and_shutdown(&self, timeout: Duration) -> bool {
        let (reply, response) = mpsc::sync_channel(1);
        if self
            .commands
            .send(TrayControlCommand::StopAndShutdown(reply))
            .is_err()
        {
            return false;
        }
        response.recv_timeout(timeout).unwrap_or(false)
    }
}

fn tray_control_loop(
    tray: &TrayRuntime,
    initialized: SyncSender<bool>,
    commands: &Receiver<TrayControlCommand>,
    busy: &AtomicBool,
) {
    let channels = spawn(false);
    if channels.requests.blocking_send(Request::Refresh).is_err() {
        let _ignored = initialized.send(false);
        return;
    }
    let mut pending = Some(PendingTrayAction::Startup(initialized));
    let mut queued_stop = None;

    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                TrayControlCommand::Toggle if pending.is_none() && queued_stop.is_none() => {
                    tray.set_busy(true);
                    let target = !tray.active();
                    if channels
                        .requests
                        .blocking_send(Request::SetActive(target))
                        .is_ok()
                    {
                        pending = Some(PendingTrayAction::Toggle);
                    } else {
                        clear_tray_busy(tray, busy);
                    }
                }
                TrayControlCommand::Toggle => {}
                TrayControlCommand::StopAndShutdown(reply) => queued_stop = Some(reply),
            }
        }

        if pending.is_none()
            && let Some(reply) = queued_stop.take()
        {
            busy.store(true, Ordering::Release);
            tray.set_busy(true);
            if channels
                .requests
                .blocking_send(Request::SetActive(false))
                .is_ok()
            {
                pending = Some(PendingTrayAction::Stop { reply, attempts: 1 });
            } else {
                let _ignored = reply.send(false);
                return;
            }
        }

        match channels.responses.recv_timeout(Duration::from_millis(33)) {
            Ok(Response::State {
                snapshot,
                request_complete,
                ..
            }) => {
                tray.set_active(snapshot.active);
                let initialization_completed =
                    matches!(pending.as_ref(), Some(PendingTrayAction::Startup(_)));
                if request_complete || initialization_completed {
                    let action = pending.take();
                    if finish_tray_action(&channels, tray, busy, action, true, snapshot.active) {
                        return;
                    }
                }
            }
            Ok(Response::Rejected {
                recovered,
                request_complete,
                ..
            }) => {
                let authoritative = recovered.is_some();
                let active = recovered
                    .as_ref()
                    .is_some_and(|(snapshot, _)| snapshot.active);
                if let Some((snapshot, _)) = recovered {
                    tray.set_active(snapshot.active);
                }
                if complete_pending_startup(&mut pending, request_complete, authoritative, active) {
                    clear_tray_busy(tray, busy);
                    continue;
                }
                if request_complete
                    && !matches!(pending.as_ref(), Some(PendingTrayAction::Startup(_)))
                {
                    if (!authoritative || active) && retry_pending_stop(&channels, &mut pending) {
                        continue;
                    }
                    let action = pending.take();
                    if finish_tray_action(&channels, tray, busy, action, authoritative, active) {
                        return;
                    }
                }
            }
            Ok(Response::Diagnostics(_) | Response::Meters(_))
            | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                fail_pending_tray_actions(pending.take(), queued_stop.take());
                return;
            }
        }
    }
}

fn clear_tray_busy(tray: &TrayRuntime, busy: &AtomicBool) {
    busy.store(false, Ordering::Release);
    tray.set_busy(false);
}

fn finish_tray_action(
    channels: &WorkerChannels,
    tray: &TrayRuntime,
    busy: &AtomicBool,
    action: Option<PendingTrayAction>,
    succeeded: bool,
    active: bool,
) -> bool {
    let should_exit = complete_tray_action(action, succeeded, active);
    clear_tray_busy(tray, busy);
    if should_exit {
        let _ignored = channels.requests.blocking_send(Request::Shutdown);
    }
    should_exit
}

fn complete_pending_startup(
    pending: &mut Option<PendingTrayAction>,
    request_complete: bool,
    succeeded: bool,
    active: bool,
) -> bool {
    if !request_complete || !matches!(pending.as_ref(), Some(PendingTrayAction::Startup(_))) {
        return false;
    }
    let action = pending.take();
    let _ignored = complete_tray_action(action, succeeded, active);
    true
}

fn retry_pending_stop(channels: &WorkerChannels, pending: &mut Option<PendingTrayAction>) -> bool {
    let Some(PendingTrayAction::Stop { attempts, .. }) = pending.as_mut() else {
        return false;
    };
    if *attempts >= 3
        || channels
            .requests
            .blocking_send(Request::SetActive(false))
            .is_err()
    {
        return false;
    }
    *attempts += 1;
    true
}

fn fail_pending_tray_actions(
    pending: Option<PendingTrayAction>,
    queued_stop: Option<SyncSender<bool>>,
) {
    match pending {
        Some(PendingTrayAction::Startup(reply) | PendingTrayAction::Stop { reply, .. }) => {
            let _ignored = reply.send(false);
        }
        Some(PendingTrayAction::Toggle) | None => {}
    }
    if let Some(reply) = queued_stop {
        let _ignored = reply.send(false);
    }
}

fn complete_tray_action(action: Option<PendingTrayAction>, succeeded: bool, active: bool) -> bool {
    match action {
        Some(PendingTrayAction::Startup(reply)) => {
            let _ignored = reply.send(succeeded);
            false
        }
        Some(PendingTrayAction::Toggle) | None => false,
        Some(PendingTrayAction::Stop { reply, .. }) => {
            let stopped = succeeded && !active;
            let _ignored = reply.send(stopped);
            true
        }
    }
}

const INITIAL_RECONNECT_BACKOFF: Duration = Duration::from_millis(250);
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(4);

async fn worker_loop(
    mut requests: tokio::sync::mpsc::Receiver<Request>,
    responses: SyncSender<Response>,
    subscribe_to_meters: bool,
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
            subscribe_to_meters,
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
    subscribe_to_meters: bool,
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
    if subscribe_to_meters && proxy.subscribe_meters().await.is_err() {
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
                    if subscribe_to_meters {
                        let _ignored = proxy.unsubscribe_meters().await;
                    }
                    return true;
                };
                if matches!(request, Request::Shutdown) {
                    if subscribe_to_meters {
                        let _ignored = proxy.unsubscribe_meters().await;
                    }
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
            signal = meter_changes.next(), if subscribe_to_meters => {
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
            start_with_noise_reduction,
            refresh,
            ..
        } => Response::State {
            snapshot,
            inputs,
            start_with_noise_reduction,
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
                Request::SetStartWithNoiseReduction(enabled) => {
                    proxy
                        .set_start_with_noise_reduction(enabled, revision)
                        .await
                }
                Request::SelectInput(stable_id) => proxy.select_input(&stable_id, revision).await,
                Request::SetSuppressionEnabled(enabled) => {
                    proxy.set_suppression_enabled(enabled, revision).await
                }
                Request::SetStrength(strength) => proxy.set_strength(strength, revision).await,
                Request::SetLatencyProfile(profile) => {
                    proxy.set_latency_profile(&profile, revision).await
                }
                Request::SetFailMode(mode) => proxy.set_fail_mode(&mode, revision).await,
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
                // This preference was added after the original version-one
                // interface. Keep core controls usable with an older native
                // daemon while package upgrades converge.
                start_with_noise_reduction: proxy
                    .get_start_with_noise_reduction()
                    .await
                    .unwrap_or(false),
                refresh: is_refresh,
                request_complete: false,
            },
            Err(_error) => Response::Rejected {
                error: communication_error(
                    "daemon-state-incomplete",
                    "Noire could not load all daemon settings.",
                    "Restart the Noire controller and background service, then retry.",
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
    } else if technical.contains(".RollbackFailed") {
        UserError::new(
            "config-rollback-failed",
            "Noire could not restore the previous audio state after saving the setting failed.",
            "Restart Noire, then check configuration permissions and free storage.",
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
        sync::{Arc, mpsc},
        time::{Duration, SystemTime},
    };

    use noire_config::{Config, ConfigStore};
    use noire_ipc::{InputDescriptor, Noire1Proxy, Snapshot};
    use noired::{
        AudioEngine, Daemon, EngineError, EngineObservation, LaunchManager, LaunchManagerError,
        LifecycleState, NoireService, claim_name, register_service,
    };

    use super::{
        INITIAL_RECONNECT_BACKOFF, MAX_RECONNECT_BACKOFF, PendingTrayAction, Request, Response,
        complete_pending_startup, complete_tray_action, mutation_error_copy, next_backoff, spawn,
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
        let rollback = mutation_error_copy("service.Error.RollbackFailed");
        assert_eq!(rollback.code, "config-rollback-failed");
        assert!(rollback.recovery.contains("Restart Noire"));
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
    fn tray_quit_acknowledges_only_authoritative_stopped_state() {
        let (stopped_sender, stopped_receiver) = mpsc::sync_channel(1);
        assert!(complete_tray_action(
            Some(PendingTrayAction::Stop {
                reply: stopped_sender,
                attempts: 1,
            }),
            true,
            false,
        ));
        assert!(stopped_receiver.recv().unwrap_or(false));

        let (active_sender, active_receiver) = mpsc::sync_channel(1);
        assert!(complete_tray_action(
            Some(PendingTrayAction::Stop {
                reply: active_sender,
                attempts: 1,
            }),
            true,
            true,
        ));
        assert!(!active_receiver.recv().unwrap_or(true));
    }

    #[test]
    fn rejected_startup_clears_pending_state_and_reports_initialization() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut pending = Some(PendingTrayAction::Startup(sender));
        assert!(!complete_pending_startup(&mut pending, false, false, false));
        assert!(pending.is_some());
        assert!(complete_pending_startup(&mut pending, true, false, false));
        assert!(pending.is_none());
        assert!(!receiver.recv().unwrap_or(true));
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
        let channels = spawn(true);

        channels.requests.blocking_send(Request::Refresh)?;
        let initial = state(receive_completed(&runtime, &channels)?)?;
        assert_eq!(initial.revision, 1);
        assert_strength(initial.strength, 0.55);

        channels
            .requests
            .blocking_send(Request::SetStartWithNoiseReduction(true))?;
        match receive_completed(&runtime, &channels)? {
            Response::State {
                snapshot,
                start_with_noise_reduction,
                ..
            } => {
                assert!(start_with_noise_reduction);
                assert!(!snapshot.active);
            }
            response => {
                return Err(format!("expected startup preference state: {response:?}").into());
            }
        }
        channels
            .requests
            .blocking_send(Request::SetStartWithNoiseReduction(false))?;
        let startup_disabled = receive_completed(&runtime, &channels)?;
        assert!(matches!(
            startup_disabled,
            Response::State {
                start_with_noise_reduction: false,
                ..
            }
        ));

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
