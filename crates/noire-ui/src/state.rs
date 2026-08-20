//! Pure presentation state shared by GPUI and headless tests.

use noire_ipc::{InputDescriptor, Metrics, Snapshot};

/// One input choice shown by the UI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputChoice {
    /// Empty for session-default selection, otherwise a stable input ID.
    pub stable_id: String,
    /// Human-readable and availability-aware label.
    pub label: String,
}

/// Stable user-facing failure produced by the UI transport or a rejected request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserError {
    /// Machine-readable support code.
    pub code: String,
    /// Plain-language cause.
    pub cause: String,
    /// Concrete next step.
    pub recovery: String,
    /// Whether the UI should offer Retry.
    pub retryable: bool,
}

impl UserError {
    /// Creates one complete error presentation contract.
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        cause: impl Into<String>,
        recovery: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            code: code.into(),
            cause: cause.into(),
            recovery: recovery.into(),
            retryable,
        }
    }
}

/// Authoritative daemon state retained by the UI.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiState {
    snapshot: Option<Snapshot>,
    inputs: Vec<InputDescriptor>,
    request_pending: bool,
    client_error: Option<UserError>,
}

/// Fully derived text and action state for a GPUI render pass.
#[derive(Clone, Debug, PartialEq)]
pub struct Presentation {
    /// Short status that never depends on color alone.
    pub status: String,
    /// Additional lifecycle, input, or recovery context.
    pub detail: String,
    /// Primary start/stop action label.
    pub primary_action: String,
    /// Whether daemon-backed settings may be changed.
    pub controls_enabled: bool,
    /// Error cause, when one needs to be presented.
    pub error_message: Option<String>,
    /// Stable support code paired with the visible error.
    pub error_code: Option<String>,
    /// Actionable recovery text paired with a daemon error.
    pub recovery: Option<String>,
    /// Whether Retry is appropriate for the current error.
    pub retryable: bool,
}

impl UiState {
    /// Marks a request in flight without changing the last authoritative values.
    pub fn set_request_pending(&mut self, pending: bool) {
        self.request_pending = pending;
    }

    /// Converges every displayed value to one daemon reply.
    pub fn converge(&mut self, snapshot: Snapshot, inputs: Vec<InputDescriptor>) {
        self.snapshot = Some(snapshot);
        self.inputs = inputs;
        self.request_pending = false;
        self.client_error = None;
    }

    /// Refreshes daemon-owned values while retaining a visible mutation error.
    ///
    /// A successful reconnect clears the transport error that accompanied the
    /// disconnected state. Ordinary daemon signals do not erase a rejection
    /// before the user can read it.
    pub fn refresh(&mut self, snapshot: Snapshot, inputs: Vec<InputDescriptor>) {
        let reconnected = self.snapshot.is_none();
        self.snapshot = Some(snapshot);
        self.inputs = inputs;
        self.request_pending = false;
        if reconnected {
            self.client_error = None;
        }
    }

    /// Records a rejected or disconnected request.
    ///
    /// If a recovery read succeeded, its daemon values replace the local values
    /// before the error is presented. A rejected setting is therefore never
    /// displayed as though it were committed.
    pub fn reject(
        &mut self,
        error: UserError,
        recovered: Option<(Snapshot, Vec<InputDescriptor>)>,
    ) {
        if let Some((snapshot, inputs)) = recovered {
            self.snapshot = Some(snapshot);
            self.inputs = inputs;
        } else {
            self.snapshot = None;
            self.inputs.clear();
        }
        self.request_pending = false;
        self.client_error = Some(error);
    }

    /// Returns the latest complete daemon snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    /// Applies one daemon-published low-rate meter snapshot without changing
    /// configuration state or clearing a visible request error.
    pub fn update_metrics(&mut self, metrics: Metrics) {
        if let Some(snapshot) = self.snapshot.as_mut() {
            snapshot.metrics = metrics;
        }
    }

    /// Reports whether a user mutation is awaiting daemon convergence.
    #[must_use]
    pub fn request_pending(&self) -> bool {
        self.request_pending
    }

    /// Returns input choices with the session default first.
    #[must_use]
    pub fn input_choices(&self) -> Vec<InputChoice> {
        let mut choices = vec![InputChoice {
            stable_id: String::new(),
            label: "Follow system default".to_owned(),
        }];
        choices.extend(self.inputs.iter().map(|input| {
            let qualifier = match (input.is_default, input.availability.as_str()) {
                (true, "available") => " — system default",
                (_, "available") => "",
                (_, "unavailable") => " — unavailable",
                _ => " — availability unknown",
            };
            InputChoice {
                stable_id: input.stable_id.clone(),
                label: format!("{}{qualifier}", input.display_name),
            }
        }));
        choices
    }

    /// Derives all status and action copy from daemon truth.
    #[must_use]
    pub fn presentation(&self) -> Presentation {
        let Some(snapshot) = &self.snapshot else {
            let client_error = self.client_error.as_ref();
            return Presentation {
                status: "Daemon unavailable".to_owned(),
                detail: client_error.map_or_else(
                    || "Noire could not reach the background service.".to_owned(),
                    |error| format!("Error code: {}", error.code),
                ),
                primary_action: "Start".to_owned(),
                controls_enabled: false,
                error_message: client_error.map(|error| error.cause.clone()),
                error_code: client_error.map(|error| error.code.clone()),
                recovery: Some(client_error.map_or_else(
                    || "Start or restart the Noire user service, then retry.".to_owned(),
                    |error| error.recovery.clone(),
                )),
                retryable: client_error.is_none_or(|error| error.retryable),
            };
        };

        let (status, detail) = lifecycle_copy(snapshot);
        let daemon_error = snapshot.has_error.then_some(&snapshot.last_error);
        Presentation {
            status,
            detail,
            primary_action: if snapshot.active { "Stop" } else { "Start" }.to_owned(),
            controls_enabled: !self.request_pending,
            error_message: self
                .client_error
                .as_ref()
                .map(|error| error.cause.clone())
                .or_else(|| daemon_error.map(|error| error.message.clone())),
            error_code: self
                .client_error
                .as_ref()
                .map(|error| error.code.clone())
                .or_else(|| daemon_error.map(|error| error.code.clone()))
                .filter(|code| !code.is_empty()),
            recovery: self
                .client_error
                .as_ref()
                .map(|error| error.recovery.clone())
                .or_else(|| daemon_error.map(|error| error.recovery.clone()))
                .filter(|recovery| !recovery.is_empty()),
            retryable: self.client_error.as_ref().map_or_else(
                || daemon_error.is_some_and(|error| error.retryable),
                |error| error.retryable,
            ),
        }
    }
}

fn lifecycle_copy(snapshot: &Snapshot) -> (String, String) {
    if snapshot.has_error {
        return (
            "Needs attention".to_owned(),
            if snapshot.last_error.code.is_empty() {
                "The daemon reported a problem.".to_owned()
            } else {
                format!("Error code: {}", snapshot.last_error.code)
            },
        );
    }

    let input = if snapshot.input_display_name.is_empty() {
        "the system default input"
    } else {
        snapshot.input_display_name.as_str()
    };
    match snapshot.state.as_str() {
        "running" => (
            "Noise reduction is active".to_owned(),
            format!("Listening to {input}."),
        ),
        "starting" | "recovering" => (
            "Reconnecting".to_owned(),
            format!("Restoring {input}; output remains safely muted until ready."),
        ),
        "stopping" => (
            "Stopping".to_owned(),
            "Removing the virtual microphone safely.".to_owned(),
        ),
        _ => (
            "Noise reduction is off".to_owned(),
            format!("Ready to use {input}."),
        ),
    }
}

#[cfg(test)]
mod tests {
    use noire_ipc::{API_VERSION, ERROR_CATALOG, ErrorInfo, Metrics, SNAPSHOT_SCHEMA_VERSION};

    use super::*;

    fn snapshot() -> Snapshot {
        Snapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            api_version: API_VERSION.to_owned(),
            build_version: env!("CARGO_PKG_VERSION").to_owned(),
            revision: 7,
            device_revision: 3,
            state: "running".to_owned(),
            active: true,
            launch_at_login: false,
            input_mode: "selected".to_owned(),
            input_stable_id: "usb:desk".to_owned(),
            input_display_name: "Desk microphone".to_owned(),
            channel: "auto".to_owned(),
            fallback_to_default: false,
            source_node_name: "io.github.rayan6ms.Noire.Microphone".to_owned(),
            latency_profile: "low".to_owned(),
            suppression_enabled: true,
            strength: 0.8,
            fail_mode: "closed".to_owned(),
            model_id: "org.noire.fastenhancer.base-48khz".to_owned(),
            model_delay_samples: 512,
            pipewire_version: "1.4.7".to_owned(),
            uptime_millis: 5,
            has_error: false,
            last_error: ErrorInfo::default(),
            metrics: Metrics::default(),
        }
    }

    fn client_error() -> UserError {
        UserError::new(
            "client-test-error",
            "The test request failed.",
            "Retry the test request.",
            true,
        )
    }

    #[test]
    fn healthy_running_state_has_complete_plain_language_status() {
        let mut state = UiState::default();
        state.converge(snapshot(), Vec::new());
        let presentation = state.presentation();
        assert_eq!(presentation.status, "Noise reduction is active");
        assert!(presentation.detail.contains("Desk microphone"));
        assert_eq!(presentation.primary_action, "Stop");
        assert!(presentation.controls_enabled);
    }

    #[test]
    fn daemon_error_exposes_code_cause_recovery_and_retry() {
        let mut value = snapshot();
        value.has_error = true;
        value.last_error = ErrorInfo {
            code: "input-unavailable".to_owned(),
            message: "The selected microphone is unavailable.".to_owned(),
            recovery: "Reconnect it or select another microphone.".to_owned(),
            retryable: true,
            ..ErrorInfo::default()
        };
        let mut state = UiState::default();
        state.converge(value, Vec::new());
        let presentation = state.presentation();
        assert_eq!(presentation.status, "Needs attention");
        assert_eq!(
            presentation.error_message.as_deref(),
            Some("The selected microphone is unavailable.")
        );
        assert!(presentation.recovery.is_some());
        assert!(presentation.retryable);
    }

    #[test]
    fn every_catalog_error_has_complete_operable_ui_presentation() {
        for entry in ERROR_CATALOG {
            let mut value = snapshot();
            value.has_error = true;
            value.last_error = ErrorInfo {
                code: entry.code.to_owned(),
                message: entry.cause.to_owned(),
                recovery: entry.recovery.to_owned(),
                component: "test".to_owned(),
                retryable: entry.retryable,
                timestamp_millis: 1,
            };
            let mut state = UiState::default();
            state.converge(value, Vec::new());
            let presentation = state.presentation();
            assert_eq!(presentation.status, "Needs attention", "{}", entry.code);
            assert_eq!(
                presentation.error_code.as_deref(),
                Some(entry.code),
                "{}",
                entry.code
            );
            assert_eq!(
                presentation.error_message.as_deref(),
                Some(entry.cause),
                "{}",
                entry.code
            );
            assert_eq!(
                presentation.recovery.as_deref(),
                Some(entry.recovery),
                "{}",
                entry.code
            );
            assert_eq!(presentation.retryable, entry.retryable, "{}", entry.code);
            assert!(presentation.controls_enabled, "{}", entry.code);
        }
    }

    #[test]
    fn rejected_mutation_restores_authoritative_snapshot() {
        let mut state = UiState::default();
        let mut old = snapshot();
        old.strength = 0.3;
        state.converge(old.clone(), Vec::new());
        state.set_request_pending(true);
        state.reject(
            UserError::new(
                "conflict",
                "The setting conflicted with another client.",
                "Retry against current daemon state.",
                true,
            ),
            Some((old, Vec::new())),
        );
        assert_eq!(state.snapshot().map(|value| value.strength), Some(0.3));
        assert_eq!(
            state.presentation().error_message.as_deref(),
            Some("The setting conflicted with another client.")
        );
        assert_eq!(state.presentation().error_code.as_deref(), Some("conflict"));
        assert_eq!(
            state.presentation().recovery.as_deref(),
            Some("Retry against current daemon state.")
        );
        state.refresh(snapshot(), Vec::new());
        assert_eq!(
            state.presentation().error_message.as_deref(),
            Some("The setting conflicted with another client.")
        );
    }

    #[test]
    fn successful_refresh_clears_a_disconnection_error() {
        let mut state = UiState::default();
        state.reject(client_error(), None);
        state.refresh(snapshot(), Vec::new());
        assert_eq!(state.presentation().error_message, None);
    }

    #[test]
    fn subscribed_meter_update_changes_only_low_rate_metrics() {
        let mut state = UiState::default();
        let original = snapshot();
        state.converge(original.clone(), Vec::new());
        state.update_metrics(Metrics {
            rms: 0.25,
            peak: 0.5,
            ..Metrics::default()
        });
        assert_eq!(
            state.snapshot().map(|updated| updated.revision),
            Some(original.revision)
        );
        assert_eq!(
            state.snapshot().map(|updated| updated.strength),
            Some(original.strength)
        );
        assert_eq!(
            state.snapshot().map(|updated| updated.metrics.rms),
            Some(0.25)
        );
        assert_eq!(
            state.snapshot().map(|updated| updated.metrics.peak),
            Some(0.5)
        );
    }

    #[test]
    fn disconnected_state_disables_every_daemon_control() {
        let mut state = UiState::default();
        state.reject(client_error(), None);
        let presentation = state.presentation();
        assert_eq!(presentation.status, "Daemon unavailable");
        assert!(!presentation.controls_enabled);
        assert!(presentation.retryable);
    }

    #[test]
    fn input_choices_are_stable_labeled_and_availability_aware() {
        let inputs = vec![
            InputDescriptor {
                stable_id: "usb:desk".to_owned(),
                display_name: "Desk microphone".to_owned(),
                is_default: true,
                availability: "available".to_owned(),
            },
            InputDescriptor {
                stable_id: "usb:travel".to_owned(),
                display_name: "Travel microphone".to_owned(),
                is_default: false,
                availability: "unavailable".to_owned(),
            },
        ];
        let mut state = UiState::default();
        state.converge(snapshot(), inputs);
        let choices = state.input_choices();
        assert_eq!(choices[0].stable_id, "");
        assert!(choices[1].label.contains("system default"));
        assert!(choices[2].label.contains("unavailable"));
    }
}
