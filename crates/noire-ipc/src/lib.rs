//! Stable control-plane types and the versioned session D-Bus contract.

#![forbid(unsafe_code)]
#![cfg_attr(feature = "dbus", allow(missing_docs))]

use serde::{Deserialize, Serialize};

#[cfg(feature = "dbus")]
use zbus::zvariant::Type;

/// Well-known per-user service name.
pub const BUS_NAME: &str = "io.github.rayan6ms.Noire.Noire1";
/// Stable root object path.
pub const OBJECT_PATH: &str = "/io/github/rayan6ms/Noire/Noire1";
/// Version-one public interface.
pub const INTERFACE_NAME: &str = "io.github.rayan6ms.Noire.Noire1";
/// JSON and typed snapshot schema version.
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;
/// Versioned API label exposed to clients and diagnostics.
pub const API_VERSION: &str = "1.0";

/// One stable public error-catalog entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ErrorCatalogEntry {
    /// Machine-readable code.
    pub code: &'static str,
    /// Plain-language cause.
    pub cause: &'static str,
    /// Actionable user recovery.
    pub recovery: &'static str,
}

/// Stable user-facing error categories covered by daemon and CLI tests.
pub const ERROR_CATALOG: &[ErrorCatalogEntry] = &[
    ErrorCatalogEntry {
        code: "conflict",
        cause: "another client committed a newer state revision",
        recovery: "refresh daemon state and retry",
    },
    ErrorCatalogEntry {
        code: "invalid-argument",
        cause: "a requested setting failed complete validation",
        recovery: "correct the rejected value and retry",
    },
    ErrorCatalogEntry {
        code: "config-persistence",
        cause: "the validated configuration could not be saved durably",
        recovery: "check configuration directory permissions and free storage",
    },
    ErrorCatalogEntry {
        code: "config-newer-schema",
        cause: "the configuration was written by a newer incompatible daemon",
        recovery: "run a daemon supporting that schema; the file remains untouched",
    },
    ErrorCatalogEntry {
        code: "input-unavailable",
        cause: "the selected or default input is not currently available",
        recovery: "select an available input or enable explicit default fallback",
    },
    ErrorCatalogEntry {
        code: "pipewire-unavailable",
        cause: "the per-user PipeWire service cannot be reached",
        recovery: "restore the user PipeWire session and retry",
    },
    ErrorCatalogEntry {
        code: "audio-command-busy",
        cause: "the bounded audio control queue is temporarily full",
        recovery: "wait briefly and retry",
    },
    ErrorCatalogEntry {
        code: "model-initialization-failed",
        cause: "the bundled suppression model could not initialize",
        recovery: "restart Noire; reinstall if the condition persists",
    },
    ErrorCatalogEntry {
        code: "launch-manager-unavailable",
        cause: "the per-user systemd manager rejected launch-at-login control",
        recovery: "verify the user systemd session and retry",
    },
];

/// Complete low-rate daemon state returned atomically.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(Type))]
#[cfg_attr(feature = "dbus", zvariant(crate = "zbus::zvariant"))]
#[allow(clippy::struct_excessive_bools)]
pub struct Snapshot {
    /// Wire/JSON schema version.
    pub schema_version: u32,
    /// Compatible D-Bus API line.
    pub api_version: String,
    /// Noire build version.
    pub build_version: String,
    /// Monotonic configuration/state revision.
    pub revision: u64,
    /// Monotonic device-list revision.
    pub device_revision: u64,
    /// Stable lifecycle state name.
    pub state: String,
    /// Persisted intended active state.
    pub active: bool,
    /// Whether launch at login is enabled.
    pub launch_at_login: bool,
    /// Input selection mode.
    pub input_mode: String,
    /// Stable selected input ID, empty when following the default.
    pub input_stable_id: String,
    /// Display name of the currently resolved input, empty when unavailable.
    pub input_display_name: String,
    /// Current channel-selection expression.
    pub channel: String,
    /// Whether fallback to the session default is permitted.
    pub fallback_to_default: bool,
    /// Stable output node name.
    pub source_node_name: String,
    /// Low or balanced latency profile.
    pub latency_profile: String,
    /// Suppression enablement.
    pub suppression_enabled: bool,
    /// Suppression wet strength.
    pub strength: f64,
    /// Closed or explicitly opted-in open failure mode.
    pub fail_mode: String,
    /// Model identity.
    pub model_id: String,
    /// Declared model delay in samples.
    pub model_delay_samples: u32,
    /// `PipeWire` runtime version, empty before connection.
    pub pipewire_version: String,
    /// Daemon uptime in milliseconds.
    pub uptime_millis: u64,
    /// Whether a public error is present.
    pub has_error: bool,
    /// Last public error, or an empty sentinel when none is present.
    pub last_error: ErrorInfo,
    /// Low-rate runtime counters and timing values.
    pub metrics: Metrics,
}

/// Stable public error details. Technical text is sanitized before construction.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(Type))]
#[cfg_attr(feature = "dbus", zvariant(crate = "zbus::zvariant"))]
pub struct ErrorInfo {
    /// Stable machine code.
    pub code: String,
    /// Plain user-safe cause.
    pub message: String,
    /// Actionable next step.
    pub recovery: String,
    /// Component that raised the error.
    pub component: String,
    /// Whether Retry can reasonably resolve it.
    pub retryable: bool,
    /// Milliseconds since the Unix epoch.
    pub timestamp_millis: u64,
}

/// Bounded low-rate metrics snapshot.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(Type))]
#[cfg_attr(feature = "dbus", zvariant(crate = "zbus::zvariant"))]
pub struct Metrics {
    /// Callback p50 duration in nanoseconds.
    pub callback_p50_ns: u64,
    /// Callback p95 duration in nanoseconds.
    pub callback_p95_ns: u64,
    /// Callback p99 duration in nanoseconds.
    pub callback_p99_ns: u64,
    /// Callback maximum duration in nanoseconds.
    pub callback_max_ns: u64,
    /// Model p50 duration in nanoseconds.
    pub model_p50_ns: u64,
    /// Model p95 duration in nanoseconds.
    pub model_p95_ns: u64,
    /// Model p99 duration in nanoseconds.
    pub model_p99_ns: u64,
    /// Model maximum duration in nanoseconds.
    pub model_max_ns: u64,
    /// Current processed-ring occupancy.
    pub ring_current_samples: u64,
    /// Processed-ring high-water occupancy.
    pub ring_high_water_samples: u64,
    /// Combined underflow count.
    pub underflows: u64,
    /// Combined overflow count.
    pub overflows: u64,
    /// Input/output malformed-buffer count.
    pub buffer_errors: u64,
    /// Model error count.
    pub model_errors: u64,
    /// Model reset count.
    pub resets: u64,
    /// Non-finite sample count.
    pub non_finite_samples: u64,
    /// Low-rate voice probability.
    pub vad_probability: f64,
    /// Low-rate RMS level.
    pub rms: f64,
    /// Low-rate absolute peak.
    pub peak: f64,
}

/// One candidate input exposed without transient registry IDs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(Type))]
#[cfg_attr(feature = "dbus", zvariant(crate = "zbus::zvariant"))]
pub struct InputDescriptor {
    /// Persistable stable selector.
    pub stable_id: String,
    /// Deduplicated user-visible label.
    pub display_name: String,
    /// Whether this is the session-manager default.
    pub is_default: bool,
    /// Available, unavailable, or unknown.
    pub availability: String,
}

/// Stable diagnostic report. It contains no audio or arbitrary environment dump.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(Type))]
#[cfg_attr(feature = "dbus", zvariant(crate = "zbus::zvariant"))]
pub struct DiagnosticReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Noire version.
    pub build_version: String,
    /// D-Bus API version.
    pub api_version: String,
    /// Current lifecycle state.
    pub state: String,
    /// Stable source node name.
    pub source_node_name: String,
    /// Selected stable ID, never a raw `PipeWire` property dump.
    pub selected_input_id: String,
    /// Last stable error code.
    pub last_error_code: String,
    /// Human command for local journal inspection.
    pub journal_hint: String,
    /// Explicit privacy statement.
    pub privacy: String,
}

/// Stable D-Bus failures for rejected mutations.
#[cfg(feature = "dbus")]
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "io.github.rayan6ms.Noire.Noire1.Error")]
pub enum ServiceError {
    /// Expected revision did not match current state.
    Conflict(String),
    /// A full candidate failed validation.
    InvalidArgument(String),
    /// Audio or input resource is unavailable.
    Unavailable(String),
    /// Durable persistence failed.
    Persistence(String),
    /// Launch-at-login manager operation failed.
    LaunchManager(String),
    /// Fixed control queue rejected work.
    Busy(String),
    /// Service could not fulfill the request safely.
    Internal(String),
    /// Transport-level D-Bus failure.
    #[zbus(error)]
    ZBus(zbus::Error),
}

/// Generated asynchronous proxy for the version-one service.
#[cfg(feature = "dbus")]
#[allow(missing_docs)]
#[zbus::proxy(
    default_service = "io.github.rayan6ms.Noire.Noire1",
    default_path = "/io/github/rayan6ms/Noire/Noire1",
    interface = "io.github.rayan6ms.Noire.Noire1"
)]
pub trait Noire1 {
    /// Returns one atomic full-state snapshot.
    fn get_snapshot(&self) -> zbus::Result<Snapshot>;
    /// Returns the immutable current device list.
    fn list_inputs(&self) -> zbus::Result<Vec<InputDescriptor>>;
    /// Starts processing if the revision is current.
    fn start(&self, expected_revision: u64) -> zbus::Result<Snapshot>;
    /// Stops processing if the revision is current.
    fn stop(&self, expected_revision: u64) -> zbus::Result<Snapshot>;
    /// Selects a stable input.
    /// An empty selector restores follow-default policy.
    fn select_input(&self, stable_id: &str, expected_revision: u64) -> zbus::Result<Snapshot>;
    /// Changes smooth suppression enablement.
    fn set_suppression_enabled(
        &self,
        enabled: bool,
        expected_revision: u64,
    ) -> zbus::Result<Snapshot>;
    /// Changes suppression strength.
    fn set_strength(&self, strength: f64, expected_revision: u64) -> zbus::Result<Snapshot>;
    /// Changes the latency profile.
    fn set_latency_profile(&self, profile: &str, expected_revision: u64) -> zbus::Result<Snapshot>;
    /// Changes the unsafe-failure policy.
    fn set_fail_mode(&self, mode: &str, expected_revision: u64) -> zbus::Result<Snapshot>;
    /// Requests an immediate recovery attempt.
    fn retry(&self, expected_revision: u64) -> zbus::Result<Snapshot>;
    /// Transactionally changes systemd user-unit enablement.
    fn set_launch_at_login(&self, enabled: bool, expected_revision: u64) -> zbus::Result<Snapshot>;
    /// Produces a sanitized diagnostics report.
    fn diagnostics(&self) -> zbus::Result<DiagnosticReport>;

    /// Current state revision as a D-Bus property.
    #[zbus(property)]
    fn state_revision(&self) -> zbus::Result<u64>;
    /// Current device revision as a D-Bus property.
    #[zbus(property)]
    fn device_revision(&self) -> zbus::Result<u64>;

    /// Emitted after state/config convergence.
    #[zbus(signal)]
    fn state_changed(&self, revision: u64) -> zbus::Result<()>;
    /// Emitted after a device snapshot changes.
    #[zbus(signal)]
    fn devices_changed(&self, device_revision: u64) -> zbus::Result<()>;
    /// Emitted once for each deduplicated public error occurrence.
    #[zbus(signal)]
    fn error_raised(&self, error: ErrorInfo) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_json_schema_is_stable_and_versioned() -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = Snapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            api_version: API_VERSION.to_owned(),
            build_version: "0.1.0".to_owned(),
            revision: 7,
            device_revision: 3,
            state: "stopped".to_owned(),
            active: false,
            launch_at_login: false,
            input_mode: "follow-default".to_owned(),
            input_stable_id: String::new(),
            input_display_name: String::new(),
            channel: "auto".to_owned(),
            fallback_to_default: false,
            source_node_name: "io.github.rayan6ms.Noire.Microphone".to_owned(),
            latency_profile: "low".to_owned(),
            suppression_enabled: true,
            strength: 1.0,
            fail_mode: "closed".to_owned(),
            model_id: "org.rnnoise.nnnoiseless.default".to_owned(),
            model_delay_samples: 480,
            pipewire_version: String::new(),
            uptime_millis: 5,
            has_error: false,
            last_error: ErrorInfo::default(),
            metrics: Metrics::default(),
        };
        let value = serde_json::to_value(&snapshot)?;
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["revision"], 7);
        assert_eq!(serde_json::from_value::<Snapshot>(value)?, snapshot);
        assert_eq!(
            serde_json::from_str::<Snapshot>(include_str!(
                "../../../data/contracts/noirectl-snapshot-v1.json"
            ))?,
            snapshot
        );
        Ok(())
    }

    #[cfg(feature = "dbus")]
    #[test]
    fn committed_introspection_tracks_wire_signatures_and_members() {
        use zbus::zvariant::Type;

        let xml =
            include_str!("../../../data/dbus-1/interfaces/io.github.rayan6ms.Noire.Noire1.xml");
        let snapshot_signature = Snapshot::SIGNATURE.to_string();
        let inputs_signature = Vec::<InputDescriptor>::SIGNATURE.to_string();
        let diagnostics_signature = DiagnosticReport::SIGNATURE.to_string();
        assert!(xml.contains(&format!("type=\"{snapshot_signature}\"")));
        assert!(xml.contains(&format!("type=\"{inputs_signature}\"")));
        assert!(xml.contains(&format!("type=\"{diagnostics_signature}\"")));
        for member in [
            "GetSnapshot",
            "ListInputs",
            "Start",
            "Stop",
            "SelectInput",
            "SetSuppressionEnabled",
            "SetStrength",
            "SetLatencyProfile",
            "SetFailMode",
            "Retry",
            "SetLaunchAtLogin",
            "Diagnostics",
            "StateChanged",
            "DevicesChanged",
            "ErrorRaised",
        ] {
            assert!(xml.contains(&format!("name=\"{member}\"")));
        }
    }

    #[test]
    fn error_catalog_codes_causes_and_recoveries_are_unique_and_actionable() {
        let mut codes = std::collections::BTreeSet::new();
        for entry in ERROR_CATALOG {
            assert!(!entry.code.is_empty());
            assert!(!entry.cause.is_empty());
            assert!(!entry.recovery.is_empty());
            assert!(codes.insert(entry.code));
        }
    }
}
