//! Platform-neutral transactional daemon state.

use std::{fmt, time::Instant};

use noire_config::{
    Config, ConfigStore, FailMode, InputMode, LatencyProfile, LoadOutcome, LoadSource,
};
use noire_ipc::{
    API_VERSION, DiagnosticReport, ErrorInfo, InputDescriptor, Metrics, SNAPSHOT_SCHEMA_VERSION,
    Snapshot, error_catalog_entry,
};
use thiserror::Error;

#[cfg(feature = "runtime")]
use crate::systemd::LaunchManager;

/// Stable daemon lifecycle visible to all clients.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LifecycleState {
    /// No graph is intended or present.
    #[default]
    Stopped,
    /// Graph and live pipeline are available.
    Running,
    /// Intended state could not currently be realized, but control remains available.
    Degraded,
}

impl fmt::Display for LifecycleState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => formatter.write_str("stopped"),
            Self::Running => formatter.write_str("running"),
            Self::Degraded => formatter.write_str("degraded"),
        }
    }
}

/// Safe low-rate view returned after applying audio intent.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineObservation {
    /// Realized lifecycle state.
    pub state: LifecycleState,
    /// Resolved input display name.
    pub input_display_name: String,
    /// `PipeWire` runtime version.
    pub pipewire_version: String,
    /// Current metrics snapshot.
    pub metrics: Metrics,
    /// Current classified runtime fault, absent while healthy or intentionally stopped.
    pub fault: Option<EngineError>,
}

impl Default for EngineObservation {
    fn default() -> Self {
        Self {
            state: LifecycleState::Stopped,
            input_display_name: String::new(),
            pipewire_version: String::new(),
            metrics: Metrics::default(),
            fault: None,
        }
    }
}

/// Publicly classifiable audio-engine failure.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct EngineError {
    /// Stable code.
    pub code: &'static str,
    /// User-safe cause.
    pub message: String,
    /// Actionable recovery.
    pub recovery: &'static str,
    /// Whether immediate retry can help.
    pub retryable: bool,
}

/// Bounded daemon-to-audio adapter.
pub trait AudioEngine: Send {
    /// Applies a fully validated complete config or leaves previous intent intact.
    ///
    /// # Errors
    ///
    /// Returns a classified backend failure without partially applying intent.
    fn apply(&mut self, config: &Config) -> Result<EngineObservation, EngineError>;
    /// Requests an immediate recovery attempt for current intent.
    ///
    /// # Errors
    ///
    /// Returns a classified backend failure when recovery cannot converge.
    fn retry(&mut self, config: &Config) -> Result<EngineObservation, EngineError> {
        self.apply(config)
    }
    /// Returns current realized state without changing processing intent.
    ///
    /// # Errors
    ///
    /// Returns a classified backend failure when current state cannot be observed.
    fn observe(&mut self, previous: &EngineObservation) -> Result<EngineObservation, EngineError> {
        Ok(previous.clone())
    }
    /// Returns stable candidate inputs without transient registry IDs.
    ///
    /// # Errors
    ///
    /// Returns a classified discovery failure while preserving the last list.
    fn inputs(&mut self) -> Result<Vec<InputDescriptor>, EngineError>;
}

/// Headless-safe placeholder used when the native feature is not selected.
#[derive(Debug, Default)]
pub struct NullAudioEngine;

impl AudioEngine for NullAudioEngine {
    fn apply(&mut self, config: &Config) -> Result<EngineObservation, EngineError> {
        if config.active {
            Err(EngineError {
                code: "audio-backend-unavailable",
                message: "the native PipeWire backend is not present in this build".to_owned(),
                recovery: "install or run the native daemon build, then retry",
                retryable: false,
            })
        } else {
            Ok(EngineObservation::default())
        }
    }

    fn inputs(&mut self) -> Result<Vec<InputDescriptor>, EngineError> {
        Ok(Vec::new())
    }
}

/// Transaction rejection with a stable D-Bus mapping.
#[derive(Debug, Error)]
pub enum ControlError {
    /// Another client committed a newer revision.
    #[error("expected revision {expected}, current revision is {current}")]
    Conflict {
        /// Client-provided revision.
        expected: u64,
        /// Authoritative revision.
        current: u64,
    },
    /// Candidate validation failed.
    #[error("invalid candidate: {0}")]
    Invalid(String),
    /// Audio state could not be applied.
    #[error("audio unavailable: {0}")]
    Audio(EngineError),
    /// Persistence failed and runtime state was rolled back.
    #[error("persistence failed: {0}")]
    Persistence(String),
    /// A newer config file has disabled writes.
    #[error("configuration belongs to a newer schema and is read-only")]
    ReadOnly,
    /// systemd manager call failed or could not be rolled back safely.
    #[error("launch manager failed: {0}")]
    LaunchManager(String),
}

/// Single authoritative state machine owned by the D-Bus service.
pub struct Daemon {
    store: ConfigStore,
    config: Config,
    writable: bool,
    revision: u64,
    device_revision: u64,
    devices: Vec<InputDescriptor>,
    engine: Box<dyn AudioEngine>,
    observation: EngineObservation,
    started_at: Instant,
    last_error: Option<ErrorInfo>,
}

impl Daemon {
    /// Loads persisted intent and composes it with an audio engine.
    ///
    /// Startup recovery never rewrites malformed or newer files. Failure to
    /// realize persisted active intent produces degraded state while D-Bus
    /// remains controllable.
    #[must_use]
    pub fn new(store: ConfigStore, mut engine: Box<dyn AudioEngine>, loaded: LoadOutcome) -> Self {
        let mut last_error = loaded.warning.as_deref().map(|_message| {
            catalog_error_info(
                match loaded.source {
                    LoadSource::NewerReadOnly => "config-newer-schema",
                    _ => "config-recovered",
                },
                "config",
            )
        });
        let observation = match engine.apply(&loaded.config) {
            Ok(observation) => observation,
            Err(error) => {
                last_error = Some(error_info(&error));
                EngineObservation {
                    state: LifecycleState::Degraded,
                    ..EngineObservation::default()
                }
            }
        };
        Self {
            store,
            config: loaded.config,
            writable: loaded.writable,
            revision: 1,
            device_revision: 0,
            devices: Vec::new(),
            engine,
            observation,
            started_at: Instant::now(),
            last_error,
        }
    }

    /// Current monotonic state revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Current monotonic device revision.
    #[must_use]
    pub const fn device_revision(&self) -> u64 {
        self.device_revision
    }

    /// Returns an atomic client snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            api_version: API_VERSION.to_owned(),
            build_version: env!("CARGO_PKG_VERSION").to_owned(),
            revision: self.revision,
            device_revision: self.device_revision,
            state: self.observation.state.to_string(),
            active: self.config.active,
            launch_at_login: self.config.launch_at_login,
            input_mode: match self.config.input.mode {
                InputMode::Selected => "selected",
                InputMode::FollowDefault => "follow-default",
            }
            .to_owned(),
            input_stable_id: self.config.input.stable_id.clone(),
            input_display_name: self.observation.input_display_name.clone(),
            channel: self.config.input.channel.to_string(),
            fallback_to_default: self.config.input.fallback_to_default,
            source_node_name: noire_pipewire::RESERVED_NODE_NAME.to_owned(),
            latency_profile: match self.config.output.latency_profile {
                LatencyProfile::Low => "low",
                LatencyProfile::Balanced => "balanced",
            }
            .to_owned(),
            suppression_enabled: self.config.suppression.enabled,
            strength: self.config.suppression.strength,
            fail_mode: match self.config.suppression.fail_mode {
                FailMode::Closed => "closed",
                FailMode::Open => "open",
            }
            .to_owned(),
            model_id: "org.rnnoise.nnnoiseless.default".to_owned(),
            model_delay_samples: 480,
            pipewire_version: self.observation.pipewire_version.clone(),
            uptime_millis: u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            has_error: self.last_error.is_some(),
            last_error: self.last_error.clone().unwrap_or_default(),
            metrics: self.observation.metrics.clone(),
        }
    }

    /// Refreshes realized audio state and returns whether public state changed.
    #[must_use]
    pub fn refresh_observation(&mut self) -> bool {
        let previous_state = self.observation.state;
        let previous_input = self.observation.input_display_name.clone();
        let previous_version = self.observation.pipewire_version.clone();
        let previous_fault = self.observation.fault.as_ref().map(|error| error.code);
        match self.engine.observe(&self.observation) {
            Ok(observation) => {
                if let Some(error) = observation.fault.as_ref() {
                    self.last_error = Some(error_info(error));
                } else if observation.state != LifecycleState::Degraded
                    && self
                        .last_error
                        .as_ref()
                        .is_some_and(|error| error.component == "audio")
                {
                    self.last_error = None;
                }
                self.observation = observation;
            }
            Err(error) => {
                self.last_error = Some(error_info(&error));
                self.observation.state = LifecycleState::Degraded;
            }
        }
        let changed = previous_state != self.observation.state
            || previous_input != self.observation.input_display_name
            || previous_version != self.observation.pipewire_version
            || previous_fault != self.observation.fault.as_ref().map(|error| error.code);
        if changed {
            self.revision = self.revision.saturating_add(1);
        }
        changed
    }

    /// Refreshes and returns the stable input list.
    ///
    /// # Errors
    ///
    /// Returns a classified engine failure while retaining the old list.
    pub fn inputs(&mut self) -> Result<Vec<InputDescriptor>, ControlError> {
        let next = self.engine.inputs().map_err(ControlError::Audio)?;
        if next != self.devices {
            self.devices = next;
            self.device_revision = self.device_revision.saturating_add(1);
        }
        Ok(self.devices.clone())
    }

    /// Atomically starts intended processing.
    ///
    /// # Errors
    ///
    /// Returns conflict, validation, audio, read-only, or persistence failure.
    pub fn start(&mut self, expected: u64) -> Result<Snapshot, ControlError> {
        self.mutate(expected, |config| config.active = true)
    }

    /// Atomically stops intended processing.
    ///
    /// # Errors
    ///
    /// Returns conflict, audio, read-only, or persistence failure.
    pub fn stop(&mut self, expected: u64) -> Result<Snapshot, ControlError> {
        self.mutate(expected, |config| config.active = false)
    }

    /// Atomically selects a stable input.
    ///
    /// # Errors
    ///
    /// Returns conflict, validation, audio, read-only, or persistence failure.
    pub fn select_input(
        &mut self,
        stable_id: String,
        expected: u64,
    ) -> Result<Snapshot, ControlError> {
        self.mutate(expected, move |config| {
            if stable_id.is_empty() {
                config.input.mode = InputMode::FollowDefault;
                config.input.stable_id.clear();
            } else {
                config.input.mode = InputMode::Selected;
                config.input.stable_id = stable_id;
            }
        })
    }

    /// Atomically changes suppression enablement.
    ///
    /// # Errors
    ///
    /// Returns conflict, audio, read-only, or persistence failure.
    pub fn set_suppression_enabled(
        &mut self,
        enabled: bool,
        expected: u64,
    ) -> Result<Snapshot, ControlError> {
        self.mutate(expected, |config| config.suppression.enabled = enabled)
    }

    /// Atomically changes suppression strength.
    ///
    /// # Errors
    ///
    /// Returns conflict, range validation, audio, read-only, or persistence failure.
    pub fn set_strength(&mut self, strength: f64, expected: u64) -> Result<Snapshot, ControlError> {
        self.mutate(expected, |config| config.suppression.strength = strength)
    }

    /// Atomically changes the latency profile.
    ///
    /// # Errors
    ///
    /// Returns conflict, audio, read-only, or persistence failure.
    pub fn set_latency_profile(
        &mut self,
        profile: LatencyProfile,
        expected: u64,
    ) -> Result<Snapshot, ControlError> {
        self.mutate(expected, |config| config.output.latency_profile = profile)
    }

    /// Atomically changes failure privacy policy.
    ///
    /// # Errors
    ///
    /// Returns conflict, audio, read-only, or persistence failure.
    pub fn set_fail_mode(
        &mut self,
        mode: FailMode,
        expected: u64,
    ) -> Result<Snapshot, ControlError> {
        self.mutate(expected, |config| config.suppression.fail_mode = mode)
    }

    /// Retries realization without silently changing intended configuration.
    ///
    /// # Errors
    ///
    /// Returns a conflict or classified audio recovery failure.
    pub fn retry(&mut self, expected: u64) -> Result<Snapshot, ControlError> {
        self.check_revision(expected)?;
        match self.engine.retry(&self.config) {
            Ok(observation) => {
                self.observation = observation;
                self.last_error = None;
                self.revision = self.revision.saturating_add(1);
                Ok(self.snapshot())
            }
            Err(error) => {
                self.last_error = Some(error_info(&error));
                self.observation.state = LifecycleState::Degraded;
                Err(ControlError::Audio(error))
            }
        }
    }

    /// Transactionally changes launch-at-login and persists only after systemd succeeds.
    ///
    /// # Errors
    ///
    /// Returns conflict, read-only, manager, persistence, or rollback failure.
    #[cfg(feature = "runtime")]
    pub async fn set_launch_at_login(
        &mut self,
        manager: &dyn LaunchManager,
        enabled: bool,
        expected: u64,
    ) -> Result<Snapshot, ControlError> {
        self.check_revision(expected)?;
        if !self.writable {
            return Err(ControlError::ReadOnly);
        }
        if self.config.launch_at_login == enabled {
            return Ok(self.snapshot());
        }
        manager
            .set_enabled(enabled)
            .await
            .map_err(|error| ControlError::LaunchManager(error.to_string()))?;
        let mut candidate = self.config.clone();
        candidate.launch_at_login = enabled;
        if let Err(error) = self.store.save(&candidate) {
            let rollback = manager.set_enabled(!enabled).await;
            let detail = rollback.map_or_else(
                |rollback| format!("{error}; systemd rollback also failed: {rollback}"),
                |()| error.to_string(),
            );
            return Err(ControlError::Persistence(detail));
        }
        self.config = candidate;
        self.revision = self.revision.saturating_add(1);
        self.last_error = None;
        Ok(self.snapshot())
    }

    /// Produces a privacy-bounded diagnostic report.
    #[must_use]
    pub fn diagnostics(&self) -> DiagnosticReport {
        DiagnosticReport {
            schema_version: 1,
            build_version: env!("CARGO_PKG_VERSION").to_owned(),
            api_version: API_VERSION.to_owned(),
            state: self.observation.state.to_string(),
            source_node_name: noire_pipewire::RESERVED_NODE_NAME.to_owned(),
            selected_input_id: self.config.input.stable_id.clone(),
            last_error_code: self
                .last_error
                .as_ref()
                .map_or_else(String::new, |error| error.code.clone()),
            journal_hint: "journalctl --user-unit=noire.service --since=-15min".to_owned(),
            privacy:
                "contains no audio, raw device properties, environment dump, or automatic upload"
                    .to_owned(),
        }
    }

    fn mutate(
        &mut self,
        expected: u64,
        update: impl FnOnce(&mut Config),
    ) -> Result<Snapshot, ControlError> {
        self.check_revision(expected)?;
        if !self.writable {
            return Err(ControlError::ReadOnly);
        }
        let previous = self.config.clone();
        let previous_observation = self.observation.clone();
        let mut candidate = previous.clone();
        update(&mut candidate);
        candidate
            .validate()
            .map_err(|error| ControlError::Invalid(error.to_string()))?;
        let observation = self.engine.apply(&candidate).map_err(|error| {
            self.last_error = Some(error_info(&error));
            ControlError::Audio(error)
        })?;
        if let Err(error) = self.store.save(&candidate) {
            let _ = self.engine.apply(&previous);
            self.observation = previous_observation;
            return Err(ControlError::Persistence(error.to_string()));
        }
        self.config = candidate;
        self.observation = observation;
        self.last_error = None;
        self.revision = self.revision.saturating_add(1);
        Ok(self.snapshot())
    }

    fn check_revision(&self, expected: u64) -> Result<(), ControlError> {
        if expected == self.revision {
            Ok(())
        } else {
            Err(ControlError::Conflict {
                expected,
                current: self.revision,
            })
        }
    }
}

fn error_info(error: &EngineError) -> ErrorInfo {
    if let Some(entry) = error_catalog_entry(error.code) {
        return ErrorInfo {
            code: entry.code.to_owned(),
            message: entry.cause.to_owned(),
            recovery: entry.recovery.to_owned(),
            component: "audio".to_owned(),
            retryable: entry.retryable,
            timestamp_millis: unix_millis(),
        };
    }
    ErrorInfo {
        code: error.code.to_owned(),
        message: error.message.clone(),
        recovery: error.recovery.to_owned(),
        component: "audio".to_owned(),
        retryable: error.retryable,
        timestamp_millis: unix_millis(),
    }
}

fn catalog_error_info(code: &str, component: &str) -> ErrorInfo {
    error_catalog_entry(code).map_or_else(
        || ErrorInfo {
            code: code.to_owned(),
            message: "Noire recovered from a configuration problem.".to_owned(),
            recovery: "Inspect the preserved configuration before retrying.".to_owned(),
            component: component.to_owned(),
            retryable: false,
            timestamp_millis: unix_millis(),
        },
        |entry| ErrorInfo {
            code: entry.code.to_owned(),
            message: entry.cause.to_owned(),
            recovery: entry.recovery.to_owned(),
            component: component.to_owned(),
            retryable: entry.retryable,
            timestamp_millis: unix_millis(),
        },
    )
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        fs,
        future::{self, Future},
        path::PathBuf,
        pin::Pin,
        sync::{Arc, Mutex},
        time::SystemTime,
    };

    use noire_config::{ConfigStore, LoadOutcome, LoadSource};

    use super::*;
    #[cfg(feature = "runtime")]
    use crate::{LaunchManager, LaunchManagerError};

    #[derive(Clone, Default)]
    struct RecordingEngine {
        applied: Arc<Mutex<Vec<Config>>>,
    }

    struct RecoveryProbeEngine {
        observation: Arc<Mutex<EngineObservation>>,
    }

    impl AudioEngine for RecordingEngine {
        fn apply(&mut self, config: &Config) -> Result<EngineObservation, EngineError> {
            self.applied
                .lock()
                .map_err(|_| test_engine_error("recording lock poisoned"))?
                .push(config.clone());
            Ok(EngineObservation {
                state: if config.active {
                    LifecycleState::Running
                } else {
                    LifecycleState::Stopped
                },
                input_display_name: "Test Microphone".to_owned(),
                ..EngineObservation::default()
            })
        }

        fn inputs(&mut self) -> Result<Vec<InputDescriptor>, EngineError> {
            Ok(vec![InputDescriptor {
                stable_id: "alsa_input.test".to_owned(),
                display_name: "Test Microphone".to_owned(),
                is_default: true,
                availability: "available".to_owned(),
            }])
        }
    }

    impl AudioEngine for RecoveryProbeEngine {
        fn apply(&mut self, _config: &Config) -> Result<EngineObservation, EngineError> {
            Err(test_engine_error("initial graph unavailable"))
        }

        fn observe(
            &mut self,
            _previous: &EngineObservation,
        ) -> Result<EngineObservation, EngineError> {
            self.observation
                .lock()
                .map_err(|_| test_engine_error("observation lock poisoned"))
                .map(|observation| observation.clone())
        }

        fn inputs(&mut self) -> Result<Vec<InputDescriptor>, EngineError> {
            Ok(Vec::new())
        }
    }

    fn test_engine_error(message: &str) -> EngineError {
        EngineError {
            code: "test-engine",
            message: message.to_owned(),
            recovery: "retry test",
            retryable: true,
        }
    }

    fn temporary_store(test: &str) -> Result<(PathBuf, ConfigStore), Box<dyn Error>> {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("noired-{test}-{}-{nonce}", std::process::id()));
        Ok((
            root.clone(),
            ConfigStore::new(root.join("noire/config.toml")),
        ))
    }

    fn defaults() -> LoadOutcome {
        LoadOutcome {
            config: Config::default(),
            source: LoadSource::Defaults,
            warning: None,
            writable: true,
        }
    }

    #[test]
    fn refreshed_observation_publishes_autonomous_recovery_without_metric_revision_churn()
    -> Result<(), Box<dyn Error>> {
        let (root, store) = temporary_store("recovery-observation")?;
        let observation = Arc::new(Mutex::new(EngineObservation {
            state: LifecycleState::Degraded,
            ..EngineObservation::default()
        }));
        let mut loaded = defaults();
        loaded.config.active = true;
        let mut daemon = Daemon::new(
            store,
            Box::new(RecoveryProbeEngine {
                observation: Arc::clone(&observation),
            }),
            loaded,
        );
        assert_eq!(daemon.snapshot().state, "degraded");
        assert!(daemon.snapshot().has_error);

        *observation
            .lock()
            .map_err(|_| "observation lock poisoned")? = EngineObservation {
            state: LifecycleState::Running,
            input_display_name: "Recovered Microphone".to_owned(),
            pipewire_version: "test-1.0".to_owned(),
            ..EngineObservation::default()
        };
        assert!(daemon.refresh_observation());
        let recovered_revision = daemon.revision();
        assert_eq!(daemon.snapshot().state, "running");
        assert!(!daemon.snapshot().has_error);

        observation
            .lock()
            .map_err(|_| "observation lock poisoned")?
            .metrics
            .callback_p50_ns = 10;
        assert!(!daemon.refresh_observation());
        assert_eq!(daemon.revision(), recovered_revision);
        assert_eq!(daemon.snapshot().metrics.callback_p50_ns, 10);
        if root.exists() {
            fs::remove_dir_all(root)?;
        }
        Ok(())
    }

    #[test]
    fn stale_clients_cannot_overwrite_newer_state() -> Result<(), Box<dyn Error>> {
        let (root, store) = temporary_store("revision")?;
        let mut daemon = Daemon::new(store, Box::new(RecordingEngine::default()), defaults());
        let first = daemon.set_strength(0.5, 1)?;
        assert_eq!(first.revision, 2);
        assert!(matches!(
            daemon.set_suppression_enabled(false, 1),
            Err(ControlError::Conflict {
                expected: 1,
                current: 2
            })
        ));
        assert!(daemon.snapshot().suppression_enabled);
        if root.exists() {
            fs::remove_dir_all(root)?;
        }
        Ok(())
    }

    #[test]
    fn restart_restores_persisted_intended_state_without_stale_audio() -> Result<(), Box<dyn Error>>
    {
        let (root, store) = temporary_store("restart")?;
        let mut first = Daemon::new(
            store.clone(),
            Box::new(RecordingEngine::default()),
            defaults(),
        );
        let started = first.start(1)?;
        assert!(started.active);
        drop(first);
        let loaded = store.load()?;
        let second = Daemon::new(store, Box::new(RecordingEngine::default()), loaded);
        assert!(second.snapshot().active);
        assert_eq!(second.snapshot().state, "running");
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn invalid_candidate_has_no_partial_effect() -> Result<(), Box<dyn Error>> {
        let (root, store) = temporary_store("invalid")?;
        let mut daemon = Daemon::new(store, Box::new(RecordingEngine::default()), defaults());
        assert!(matches!(
            daemon.set_strength(f64::NAN, 1),
            Err(ControlError::Invalid(_))
        ));
        assert_eq!(daemon.snapshot().revision, 1);
        assert!((daemon.snapshot().strength - 1.0).abs() < f64::EPSILON);
        if root.exists() {
            fs::remove_dir_all(root)?;
        }
        Ok(())
    }

    #[cfg(feature = "runtime")]
    #[derive(Default)]
    struct RecordingLaunchManager {
        calls: Mutex<Vec<bool>>,
    }

    #[cfg(feature = "runtime")]
    impl LaunchManager for RecordingLaunchManager {
        fn set_enabled<'a>(
            &'a self,
            enabled: bool,
        ) -> Pin<Box<dyn Future<Output = Result<(), LaunchManagerError>> + Send + 'a>> {
            let result = self
                .calls
                .lock()
                .map(|mut calls| calls.push(enabled))
                .map_err(|_| LaunchManagerError {
                    code: "test-lock",
                    message: "test launch lock poisoned".to_owned(),
                });
            Box::pin(future::ready(result))
        }
    }

    #[cfg(feature = "runtime")]
    #[tokio::test]
    async fn launch_at_login_persists_only_after_manager_success() -> Result<(), Box<dyn Error>> {
        let (root, store) = temporary_store("launch")?;
        let mut daemon = Daemon::new(
            store.clone(),
            Box::new(RecordingEngine::default()),
            defaults(),
        );
        let manager = RecordingLaunchManager::default();
        let snapshot = daemon.set_launch_at_login(&manager, true, 1).await?;
        assert!(snapshot.launch_at_login);
        assert!(store.load()?.config.launch_at_login);
        assert_eq!(manager.calls.into_inner()?, vec![true]);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[cfg(feature = "runtime")]
    #[tokio::test]
    async fn launch_at_login_rolls_systemd_back_when_persistence_fails()
    -> Result<(), Box<dyn Error>> {
        let (root, _) = temporary_store("launch-rollback")?;
        fs::create_dir_all(&root)?;
        let blocker = root.join("blocked");
        fs::write(&blocker, "not a directory")?;
        let store = ConfigStore::new(blocker.join("config.toml"));
        let mut daemon = Daemon::new(store, Box::new(RecordingEngine::default()), defaults());
        let manager = RecordingLaunchManager::default();
        assert!(matches!(
            daemon.set_launch_at_login(&manager, true, 1).await,
            Err(ControlError::Persistence(_))
        ));
        assert!(!daemon.snapshot().launch_at_login);
        assert_eq!(manager.calls.into_inner()?, vec![true, false]);
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
