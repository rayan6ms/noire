//! Version-one D-Bus service adapter.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use noire_config::{FailMode, LatencyProfile};
use noire_ipc::{
    BUS_NAME, DiagnosticReport, ErrorInfo, InputDescriptor, OBJECT_PATH, ServiceError, Snapshot,
};
use tokio::sync::Mutex;
use zbus::object_server::SignalEmitter;

use crate::{ControlError, Daemon, EventRateLimiter, LaunchManager};

/// Shared D-Bus object; the daemon remains alive independently of clients.
#[derive(Clone)]
pub struct NoireService {
    daemon: Arc<Mutex<Daemon>>,
    launch_manager: Arc<dyn LaunchManager>,
    error_limiter: Arc<Mutex<EventRateLimiter>>,
}

impl std::fmt::Debug for NoireService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NoireService")
            .finish_non_exhaustive()
    }
}

impl NoireService {
    /// Wraps the authoritative daemon and launch manager.
    #[must_use]
    pub fn new(daemon: Daemon, launch_manager: Arc<dyn LaunchManager>) -> Self {
        Self {
            daemon: Arc::new(Mutex::new(daemon)),
            launch_manager,
            error_limiter: Arc::new(Mutex::new(EventRateLimiter::new(Duration::from_secs(5)))),
        }
    }

    async fn publish(
        &self,
        result: Result<Snapshot, ControlError>,
        emitter: &SignalEmitter<'_>,
    ) -> Result<Snapshot, ServiceError> {
        match result {
            Ok(snapshot) => {
                Self::state_changed(emitter, snapshot.revision).await?;
                Ok(snapshot)
            }
            Err(error) => {
                let (event, info) = public_error(&error);
                if self
                    .error_limiter
                    .lock()
                    .await
                    .should_emit(event, Instant::now())
                {
                    tracing::warn!(event, code = info.code, "control request rejected");
                    let _ = Self::error_raised(emitter, info).await;
                }
                Err(map_error(error))
            }
        }
    }
}

#[zbus::interface(name = "io.github.rayan6ms.Noire.Noire1")]
impl NoireService {
    async fn get_snapshot(&self) -> Snapshot {
        self.daemon.lock().await.snapshot()
    }

    async fn list_inputs(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<Vec<InputDescriptor>, ServiceError> {
        let mut daemon = self.daemon.lock().await;
        let before = daemon.device_revision();
        let inputs = daemon.inputs().map_err(map_error)?;
        let after = daemon.device_revision();
        drop(daemon);
        if after != before {
            Self::devices_changed(&emitter, after).await?;
        }
        Ok(inputs)
    }

    async fn start(
        &self,
        expected_revision: u64,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<Snapshot, ServiceError> {
        let result = self.daemon.lock().await.start(expected_revision);
        self.publish(result, &emitter).await
    }

    async fn stop(
        &self,
        expected_revision: u64,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<Snapshot, ServiceError> {
        let result = self.daemon.lock().await.stop(expected_revision);
        self.publish(result, &emitter).await
    }

    async fn select_input(
        &self,
        stable_id: &str,
        expected_revision: u64,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<Snapshot, ServiceError> {
        let result = self
            .daemon
            .lock()
            .await
            .select_input(stable_id.to_owned(), expected_revision);
        self.publish(result, &emitter).await
    }

    async fn set_suppression_enabled(
        &self,
        enabled: bool,
        expected_revision: u64,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<Snapshot, ServiceError> {
        let result = self
            .daemon
            .lock()
            .await
            .set_suppression_enabled(enabled, expected_revision);
        self.publish(result, &emitter).await
    }

    async fn set_strength(
        &self,
        strength: f64,
        expected_revision: u64,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<Snapshot, ServiceError> {
        let result = self
            .daemon
            .lock()
            .await
            .set_strength(strength, expected_revision);
        self.publish(result, &emitter).await
    }

    async fn set_latency_profile(
        &self,
        profile: &str,
        expected_revision: u64,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<Snapshot, ServiceError> {
        let profile = match profile {
            "low" => LatencyProfile::Low,
            "balanced" => LatencyProfile::Balanced,
            _ => {
                return Err(ServiceError::InvalidArgument(
                    "latency profile must be low or balanced".to_owned(),
                ));
            }
        };
        let result = self
            .daemon
            .lock()
            .await
            .set_latency_profile(profile, expected_revision);
        self.publish(result, &emitter).await
    }

    async fn set_fail_mode(
        &self,
        mode: &str,
        expected_revision: u64,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<Snapshot, ServiceError> {
        let mode = match mode {
            "closed" => FailMode::Closed,
            "open" => FailMode::Open,
            _ => {
                return Err(ServiceError::InvalidArgument(
                    "fail mode must be closed or open".to_owned(),
                ));
            }
        };
        let result = self
            .daemon
            .lock()
            .await
            .set_fail_mode(mode, expected_revision);
        self.publish(result, &emitter).await
    }

    async fn retry(
        &self,
        expected_revision: u64,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<Snapshot, ServiceError> {
        let result = self.daemon.lock().await.retry(expected_revision);
        self.publish(result, &emitter).await
    }

    async fn set_launch_at_login(
        &self,
        enabled: bool,
        expected_revision: u64,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<Snapshot, ServiceError> {
        let manager = Arc::clone(&self.launch_manager);
        let result = self
            .daemon
            .lock()
            .await
            .set_launch_at_login(manager.as_ref(), enabled, expected_revision)
            .await;
        self.publish(result, &emitter).await
    }

    async fn diagnostics(&self) -> DiagnosticReport {
        self.daemon.lock().await.diagnostics()
    }

    #[zbus(property)]
    async fn state_revision(&self) -> u64 {
        self.daemon.lock().await.revision()
    }

    #[zbus(property)]
    async fn device_revision(&self) -> u64 {
        self.daemon.lock().await.device_revision()
    }

    #[zbus(signal)]
    async fn state_changed(emitter: &SignalEmitter<'_>, revision: u64) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn devices_changed(emitter: &SignalEmitter<'_>, device_revision: u64)
    -> zbus::Result<()>;

    #[zbus(signal)]
    async fn error_raised(emitter: &SignalEmitter<'_>, error: ErrorInfo) -> zbus::Result<()>;
}

/// Claims the well-known name without queueing or replacing another daemon.
///
/// # Errors
///
/// Returns `NameTaken` for a concurrent daemon.
pub async fn claim_name(connection: &zbus::Connection) -> zbus::Result<()> {
    use zbus::fdo::RequestNameFlags;

    connection
        .request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())
        .await
        .map(|_| ())
}

/// Registers the object after name ownership has been established.
///
/// # Errors
///
/// Returns an object-server registration failure.
pub async fn register_service(
    connection: &zbus::Connection,
    service: NoireService,
) -> zbus::Result<()> {
    connection.object_server().at(OBJECT_PATH, service).await?;
    Ok(())
}

fn map_error(error: ControlError) -> ServiceError {
    match error {
        ControlError::Conflict { .. } => ServiceError::Conflict(error.to_string()),
        ControlError::Invalid(_) => ServiceError::InvalidArgument(error.to_string()),
        ControlError::Audio(engine) if engine.code == "audio-command-busy" => {
            ServiceError::Busy(engine.to_string())
        }
        ControlError::Audio(engine) => ServiceError::Unavailable(engine.to_string()),
        ControlError::Persistence(_) | ControlError::ReadOnly => {
            ServiceError::Persistence(error.to_string())
        }
        ControlError::LaunchManager(_) => ServiceError::LaunchManager(error.to_string()),
    }
}

fn public_error(error: &ControlError) -> (&'static str, ErrorInfo) {
    let (event, code, recovery, component, retryable) = match error {
        ControlError::Conflict { .. } => (
            "control.conflict",
            "conflict",
            "refresh daemon state and retry against its current revision",
            "control",
            true,
        ),
        ControlError::Invalid(_) => (
            "control.invalid",
            "invalid-argument",
            "correct the rejected value and retry",
            "control",
            false,
        ),
        ControlError::Audio(engine) => (
            "audio.unavailable",
            engine.code,
            engine.recovery,
            "audio",
            engine.retryable,
        ),
        ControlError::Persistence(_) => (
            "config.persistence",
            "config-persistence",
            "check configuration directory permissions and available storage",
            "config",
            true,
        ),
        ControlError::ReadOnly => (
            "config.read-only",
            "config-newer-schema",
            "run a daemon version supporting the existing configuration",
            "config",
            false,
        ),
        ControlError::LaunchManager(_) => (
            "systemd.manager",
            "launch-manager-unavailable",
            "verify the per-user systemd manager and retry",
            "systemd",
            true,
        ),
    };
    (
        event,
        ErrorInfo {
            code: code.to_owned(),
            message: error.to_string(),
            recovery: recovery.to_owned(),
            component: component.to_owned(),
            retryable,
            timestamp_millis: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| {
                    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
                }),
        },
    )
}
