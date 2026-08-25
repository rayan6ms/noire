//! Version-one D-Bus service adapter.

use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use noire_config::{FailMode, LatencyProfile};
use noire_ipc::{
    BUS_NAME, DiagnosticReport, ErrorInfo, InputDescriptor, Metrics, OBJECT_PATH, ServiceError,
    Snapshot, error_catalog_entry,
};
use tokio::sync::Mutex;
use zbus::{message::Header, object_server::SignalEmitter};

use crate::{ControlError, Daemon, EventRateLimiter, LaunchManager};

/// Shared D-Bus object; the daemon remains alive independently of clients.
#[derive(Clone)]
pub struct NoireService {
    daemon: Arc<Mutex<Daemon>>,
    launch_manager: Arc<dyn LaunchManager>,
    error_limiter: Arc<Mutex<EventRateLimiter>>,
    meter_subscribers: Arc<Mutex<BTreeSet<String>>>,
    meter_monitoring: Arc<AtomicBool>,
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
            meter_subscribers: Arc::new(Mutex::new(BTreeSet::new())),
            meter_monitoring: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Publishes autonomous state/device changes and subscribed meters.
    ///
    /// This control-plane loop never runs on a `PipeWire` callback. Its 40 ms
    /// cadence is the maximum public meter rate, and meters are not emitted when
    /// no D-Bus client has explicitly subscribed.
    pub async fn monitor(&self, connection: &zbus::Connection) {
        let Ok(emitter) = SignalEmitter::new(connection, OBJECT_PATH) else {
            return;
        };
        let mut interval = tokio::time::interval(Duration::from_millis(40));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut prune_tick = 0_u8;

        loop {
            tokio::select! {
                () = connection.closed() => break,
                _ = interval.tick() => {}
            }

            let (state_changed, device_changed, snapshot) = {
                let mut daemon = self.daemon.lock().await;
                let before = daemon.device_revision();
                let state_changed = daemon.refresh_observation();
                let device_changed = before != daemon.device_revision();
                (state_changed, device_changed, daemon.snapshot())
            };
            if state_changed {
                let _ignored = Self::state_changed(&emitter, snapshot.revision).await;
            }
            if device_changed {
                let _ignored = Self::devices_changed(&emitter, snapshot.device_revision).await;
            }

            prune_tick = prune_tick.wrapping_add(1);
            if prune_tick >= 10 {
                self.prune_meter_subscribers(connection).await;
                prune_tick = 0;
            }
            let subscribers: Vec<String> = self
                .meter_subscribers
                .lock()
                .await
                .iter()
                .cloned()
                .collect();
            for subscriber in subscribers {
                let Ok(destination) = zbus::names::BusName::try_from(subscriber.as_str()) else {
                    continue;
                };
                let targeted = emitter.clone().set_destination(destination);
                let _ignored = Self::meters_changed(&targeted, snapshot.metrics.clone()).await;
            }
        }
    }

    async fn prune_meter_subscribers(&self, connection: &zbus::Connection) {
        let subscribers: Vec<String> = self
            .meter_subscribers
            .lock()
            .await
            .iter()
            .cloned()
            .collect();
        let mut disconnected = Vec::new();
        if !subscribers.is_empty() {
            let Ok(bus) = zbus::fdo::DBusProxy::new(connection).await else {
                return;
            };
            for subscriber in subscribers {
                let Ok(name) = zbus::names::BusName::try_from(subscriber.as_str()) else {
                    continue;
                };
                if !matches!(bus.name_has_owner(name).await, Ok(true)) {
                    disconnected.push(subscriber);
                }
            }
        }
        let mut subscribers = self.meter_subscribers.lock().await;
        for subscriber in disconnected {
            subscribers.remove(&subscriber);
        }
        if subscribers.is_empty()
            && self.meter_monitoring.load(Ordering::Acquire)
            && self.daemon.lock().await.set_meter_monitoring(false).is_ok()
        {
            self.meter_monitoring.store(false, Ordering::Release);
        }
    }

    fn caller(header: &Header<'_>) -> Result<String, ServiceError> {
        header.sender().map(ToString::to_string).ok_or_else(|| {
            ServiceError::Internal("D-Bus request did not identify its sender".to_owned())
        })
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
    async fn get_snapshot(&self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> Snapshot {
        let mut daemon = self.daemon.lock().await;
        let changed = daemon.refresh_observation();
        let snapshot = daemon.snapshot();
        drop(daemon);
        if changed {
            let _ = Self::state_changed(&emitter, snapshot.revision).await;
        }
        snapshot
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

    async fn get_start_with_noise_reduction(&self) -> bool {
        self.daemon.lock().await.start_with_noise_reduction()
    }

    async fn set_start_with_noise_reduction(
        &self,
        enabled: bool,
        expected_revision: u64,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<Snapshot, ServiceError> {
        let result = self
            .daemon
            .lock()
            .await
            .set_start_with_noise_reduction(enabled, expected_revision);
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
        let mut daemon = self.daemon.lock().await;
        let _ = daemon.refresh_observation();
        daemon.diagnostics()
    }

    async fn subscribe_meters(
        &self,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), ServiceError> {
        let caller = Self::caller(&header)?;
        let mut subscribers = self.meter_subscribers.lock().await;
        let inserted = subscribers.insert(caller.clone());
        if inserted && !self.meter_monitoring.load(Ordering::Acquire) {
            if let Err(error) = self.daemon.lock().await.set_meter_monitoring(true) {
                subscribers.remove(&caller);
                return Err(map_error(error));
            }
            self.meter_monitoring.store(true, Ordering::Release);
        }
        Ok(())
    }

    async fn unsubscribe_meters(
        &self,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), ServiceError> {
        let caller = Self::caller(&header)?;
        let mut subscribers = self.meter_subscribers.lock().await;
        let last_subscriber = subscribers.remove(&caller) && subscribers.is_empty();
        if last_subscriber && self.meter_monitoring.load(Ordering::Acquire) {
            if let Err(error) = self.daemon.lock().await.set_meter_monitoring(false) {
                subscribers.insert(caller);
                return Err(map_error(error));
            }
            self.meter_monitoring.store(false, Ordering::Release);
        }
        Ok(())
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

    #[zbus(signal)]
    async fn meters_changed(emitter: &SignalEmitter<'_>, metrics: Metrics) -> zbus::Result<()>;
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

/// Registers the object before name ownership makes the service reachable.
///
/// # Errors
///
/// Returns an object-server registration failure. Production startup must call
/// this before [`claim_name`] so activation requests cannot enter a name/object
/// registration gap.
pub async fn register_service(
    connection: &zbus::Connection,
    service: NoireService,
) -> zbus::Result<()> {
    connection.object_server().at(OBJECT_PATH, service).await?;
    Ok(())
}

/// Registers the service object and only then exposes its well-known name.
///
/// # Errors
///
/// Returns an object-server registration or non-queued name-ownership failure.
pub async fn register_and_claim(
    connection: &zbus::Connection,
    service: NoireService,
) -> zbus::Result<()> {
    register_service(connection, service).await?;
    claim_name(connection).await
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
        ControlError::PersistenceRollback { .. } => ServiceError::RollbackFailed(error.to_string()),
        ControlError::LaunchManager(_) => ServiceError::LaunchManager(error.to_string()),
    }
}

fn public_error(error: &ControlError) -> (&'static str, ErrorInfo) {
    let (event, code, component) = match error {
        ControlError::Conflict { .. } => ("control.conflict", "conflict", "control"),
        ControlError::Invalid(_) => ("control.invalid", "invalid-argument", "control"),
        ControlError::Audio(engine) => ("audio.unavailable", engine.code, "audio"),
        ControlError::Persistence(_) => ("config.persistence", "config-persistence", "config"),
        ControlError::PersistenceRollback { .. } => {
            ("config.rollback-failed", "config-rollback-failed", "audio")
        }
        ControlError::ReadOnly => ("config.read-only", "config-newer-schema", "config"),
        ControlError::LaunchManager(_) => {
            ("systemd.manager", "launch-manager-unavailable", "systemd")
        }
    };
    let (message, recovery, retryable) = error_catalog_entry(code).map_or_else(
        || match error {
            ControlError::Audio(engine) => (
                engine.message.clone(),
                engine.recovery.to_owned(),
                engine.retryable,
            ),
            _ => (
                "Noire rejected the requested change.".to_owned(),
                "Refresh daemon state and retry.".to_owned(),
                false,
            ),
        },
        |catalog| {
            (
                catalog.cause.to_owned(),
                catalog.recovery.to_owned(),
                catalog.retryable,
            )
        },
    );
    (
        event,
        ErrorInfo {
            code: code.to_owned(),
            message,
            recovery,
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

#[cfg(test)]
mod tests {
    use noire_ipc::error_catalog_entry;

    use super::*;
    use crate::EngineError;

    #[test]
    fn every_production_public_error_code_has_catalog_copy() {
        fn literal_codes(source: &str) -> impl Iterator<Item = &str> {
            let production = source
                .split_once("#[cfg(test)]")
                .map_or(source, |(production, _tests)| production);
            production
                .split("code: \"")
                .skip(1)
                .filter_map(|suffix| suffix.split_once('"').map(|(code, _rest)| code))
        }

        let sources = [
            include_str!("control.rs"),
            include_str!("native.rs"),
            include_str!("systemd.rs"),
        ];
        let mut production_codes: std::collections::BTreeSet<&str> = sources
            .iter()
            .flat_map(|source| literal_codes(source))
            .collect();
        production_codes.extend([
            "conflict",
            "invalid-argument",
            "config-persistence",
            "config-rollback-failed",
            "config-newer-schema",
            "config-recovered",
        ]);

        let uncataloged: Vec<_> = production_codes
            .into_iter()
            .filter(|code| error_catalog_entry(code).is_none())
            .collect();
        assert!(
            uncataloged.is_empty(),
            "production public errors missing catalog copy: {uncataloged:?}"
        );
    }

    #[test]
    fn rejected_requests_publish_exact_catalog_cause_and_recovery() {
        let errors = [
            ControlError::Conflict {
                expected: 1,
                current: 2,
            },
            ControlError::Invalid("test invalid value".to_owned()),
            ControlError::Audio(EngineError {
                code: "audio-command-busy",
                message: "test queue detail".to_owned(),
                recovery: "test recovery",
                retryable: false,
            }),
            ControlError::Persistence("test filesystem detail".to_owned()),
            ControlError::PersistenceRollback {
                persistence: "test filesystem detail".to_owned(),
                rollback: EngineError {
                    code: "audio-stream-failed",
                    message: "test rollback detail".to_owned(),
                    recovery: "restart test",
                    retryable: false,
                },
            },
            ControlError::ReadOnly,
            ControlError::LaunchManager("test manager detail".to_owned()),
        ];

        for error in errors {
            let (_event, public) = public_error(&error);
            let presented = (
                public.message.as_str(),
                public.recovery.as_str(),
                public.retryable,
            );
            assert_eq!(
                error_catalog_entry(&public.code).map(|catalog| (
                    catalog.cause,
                    catalog.recovery,
                    catalog.retryable
                )),
                Some(presented),
                "{}",
                public.code
            );
            assert!(!public.component.is_empty(), "{}", public.code);
        }
    }

    #[test]
    fn rollback_failure_uses_its_dedicated_dbus_error() {
        let error = ControlError::PersistenceRollback {
            persistence: "save failed".to_owned(),
            rollback: EngineError {
                code: "audio-stream-failed",
                message: "rollback failed".to_owned(),
                recovery: "restart test",
                retryable: false,
            },
        };

        assert!(matches!(map_error(error), ServiceError::RollbackFailed(_)));
    }
}
