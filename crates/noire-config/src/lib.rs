//! Versioned configuration, validation, migration, and durable persistence.

#![forbid(unsafe_code)]

use std::{
    env, fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

/// Current on-disk configuration schema.
pub const CONFIG_SCHEMA_VERSION: u32 = 1;
/// Relative configuration location below the XDG configuration directory.
pub const CONFIG_RELATIVE_PATH: &str = "noire/config.toml";

/// Complete schema-v1 configuration owned by the daemon.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Schema version written into the file.
    pub schema_version: u32,
    /// Intended processing state restored on daemon restart.
    pub active: bool,
    /// Whether the systemd user unit is enabled for login.
    pub launch_at_login: bool,
    /// Input selection policy.
    pub input: InputConfig,
    /// Output latency policy.
    pub output: OutputConfig,
    /// Suppression controls.
    pub suppression: SuppressionConfig,
    /// Low-rate diagnostic controls.
    pub diagnostics: DiagnosticsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            active: false,
            launch_at_login: false,
            input: InputConfig::default(),
            output: OutputConfig::default(),
            suppression: SuppressionConfig::default(),
            diagnostics: DiagnosticsConfig::default(),
        }
    }
}

/// Input identity and fallback policy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputConfig {
    /// Select one stable input or follow the session default.
    pub mode: InputMode,
    /// Stable `PipeWire` node ID. Empty only in follow-default mode.
    pub stable_id: String,
    /// Channel mapping into canonical mono.
    pub channel: ChannelSelection,
    /// Permit default fallback when a selected node is absent.
    pub fallback_to_default: bool,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            mode: InputMode::FollowDefault,
            stable_id: String::new(),
            channel: ChannelSelection::Auto,
            fallback_to_default: false,
        }
    }
}

/// Persisted input-selection mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputMode {
    /// Use the explicitly selected stable ID.
    Selected,
    /// Track the session-manager default input.
    #[default]
    FollowDefault,
}

/// Explicit channel mapping.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ChannelSelection {
    /// Choose mono or a safe downmix from negotiated metadata.
    #[default]
    Auto,
    /// Require mono input.
    Mono,
    /// Select the left channel.
    Left,
    /// Select the right channel.
    Right,
    /// Select a zero-based interleaved channel.
    Index(u16),
}

impl fmt::Display for ChannelSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => formatter.write_str("auto"),
            Self::Mono => formatter.write_str("mono"),
            Self::Left => formatter.write_str("left"),
            Self::Right => formatter.write_str("right"),
            Self::Index(index) => write!(formatter, "index:{index}"),
        }
    }
}

impl Serialize for ChannelSelection {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ChannelSelection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "auto" => Ok(Self::Auto),
            "mono" => Ok(Self::Mono),
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            _ => value
                .strip_prefix("index:")
                .and_then(|index| index.parse().ok())
                .map(Self::Index)
                .ok_or_else(|| de::Error::custom("expected auto, mono, left, right, or index:N")),
        }
    }
}

/// Output settings.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    /// Stream latency tradeoff.
    pub latency_profile: LatencyProfile,
}

/// Supported stream latency profiles.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LatencyProfile {
    /// Minimum supported buffering.
    #[default]
    Low,
    /// More buffering for less scheduling-sensitive systems.
    Balanced,
}

/// Model and failure controls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuppressionConfig {
    /// Whether processed rather than dry delayed samples are mixed.
    pub enabled: bool,
    /// Wet strength in the inclusive range zero through one.
    pub strength: f64,
    /// Behavior after an unsafe model failure.
    pub fail_mode: FailMode,
}

impl Default for SuppressionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            strength: 1.0,
            fail_mode: FailMode::Closed,
        }
    }
}

/// Explicit unsafe-failure policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailMode {
    /// Stop new output and ramp to silence.
    #[default]
    Closed,
    /// Use latency-matched dry audio after explicit opt-in.
    Open,
}

/// Diagnostic settings.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsConfig {
    /// Maximum emitted log level.
    pub log_level: LogLevel,
    /// Whether low-rate meter snapshots may be published.
    pub metering: bool,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            log_level: LogLevel::Info,
            metering: true,
        }
    }
}

/// Supported stable logging levels.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Errors only.
    Error,
    /// Warnings and errors.
    Warn,
    /// Lifecycle information, warnings, and errors.
    #[default]
    Info,
    /// Diagnostic control-plane events.
    Debug,
}

/// Stable validation failure suitable for UI and CLI presentation.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{path}: {message}")]
pub struct ValidationError {
    /// Stable machine-readable code.
    pub code: &'static str,
    /// Full configuration path.
    pub path: String,
    /// User-safe explanation.
    pub message: String,
}

impl Config {
    /// Validates the entire candidate before any effect is applied.
    ///
    /// # Errors
    ///
    /// Returns the first stable, path-addressed validation failure.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(validation(
                "unsupported-schema",
                "schema_version",
                "must equal 1",
            ));
        }
        if self.input.mode == InputMode::Selected && self.input.stable_id.trim().is_empty() {
            return Err(validation(
                "input-id-required",
                "input.stable_id",
                "must be non-empty when input.mode is selected",
            ));
        }
        if self.input.mode == InputMode::FollowDefault && !self.input.stable_id.is_empty() {
            return Err(validation(
                "input-id-unexpected",
                "input.stable_id",
                "must be empty when input.mode is follow-default",
            ));
        }
        if !self.suppression.strength.is_finite()
            || !(0.0..=1.0).contains(&self.suppression.strength)
        {
            return Err(validation(
                "strength-out-of-range",
                "suppression.strength",
                "must be finite and in the inclusive range 0.0 through 1.0",
            ));
        }
        Ok(())
    }

    /// Returns canonical schema-v1 TOML after full validation.
    ///
    /// # Errors
    ///
    /// Returns validation or serialization errors.
    pub fn to_canonical_toml(&self) -> Result<String, ConfigError> {
        self.validate()?;
        toml::to_string_pretty(self).map_err(ConfigError::Serialize)
    }
}

fn validation(code: &'static str, path: &str, message: &str) -> ValidationError {
    ValidationError {
        code,
        path: path.to_owned(),
        message: message.to_owned(),
    }
}

/// Source selected while loading configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadSource {
    /// No file existed and defaults were selected.
    Defaults,
    /// The authoritative file was valid.
    Primary,
    /// The primary file was malformed and a valid backup was recovered.
    Backup,
    /// No valid file existed, so safe defaults were retained.
    SafeDefaults,
    /// A newer schema was preserved read-only and safe defaults were retained.
    NewerReadOnly,
}

/// Non-destructive configuration load result.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadOutcome {
    /// Valid configuration used by the daemon.
    pub config: Config,
    /// File/default source of the configuration.
    pub source: LoadSource,
    /// User-safe warning when recovery was needed.
    pub warning: Option<String>,
    /// Whether mutations may be persisted.
    pub writable: bool,
}

/// Persistence or schema failure.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Full candidate validation failed.
    #[error(transparent)]
    Validation(#[from] ValidationError),
    /// TOML parsing failed.
    #[error("configuration parse failed: {0}")]
    Parse(#[from] toml::de::Error),
    /// TOML serialization failed.
    #[error("configuration serialization failed: {0}")]
    Serialize(toml::ser::Error),
    /// Filesystem durability operation failed.
    #[error("configuration I/O failed at {path}: {source}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Underlying failure.
        source: io::Error,
    },
    /// The file belongs to a newer daemon and must not be rewritten.
    #[error("configuration schema {found} is newer than supported schema {supported}")]
    NewerSchema {
        /// Version found on disk.
        found: u64,
        /// Maximum supported version.
        supported: u32,
    },
    /// Schema version is missing or cannot be represented.
    #[error("configuration schema_version is missing or invalid")]
    InvalidSchemaVersion,
}

/// Authoritative config file plus its same-directory last-known-good backup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigStore {
    path: PathBuf,
    backup_path: PathBuf,
}

impl ConfigStore {
    /// Creates a store for an explicit authoritative path.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let backup_path = path.with_extension("toml.bak");
        Self { path, backup_path }
    }

    /// Resolves the XDG configuration path without creating it.
    ///
    /// # Errors
    ///
    /// Returns an I/O-shaped error when neither `XDG_CONFIG_HOME` nor `HOME` is usable.
    pub fn discover() -> Result<Self, ConfigError> {
        let root = env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("HOME")
                    .filter(|value| !value.is_empty())
                    .map(|home| PathBuf::from(home).join(".config"))
            })
            .ok_or_else(|| ConfigError::Io {
                path: PathBuf::from(CONFIG_RELATIVE_PATH),
                source: io::Error::new(io::ErrorKind::NotFound, "no configuration home"),
            })?;
        Ok(Self::new(root.join(CONFIG_RELATIVE_PATH)))
    }

    /// Authoritative path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Last-known-good backup path.
    #[must_use]
    pub fn backup_path(&self) -> &Path {
        &self.backup_path
    }

    /// Loads valid state without ever modifying malformed or newer input.
    ///
    /// # Errors
    ///
    /// Returns only filesystem errors that prevent determining file state.
    pub fn load(&self) -> Result<LoadOutcome, ConfigError> {
        let primary = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(LoadOutcome {
                    config: Config::default(),
                    source: LoadSource::Defaults,
                    warning: None,
                    writable: true,
                });
            }
            Err(source) => return Err(io_error(&self.path, source)),
        };

        match decode_and_migrate(&primary) {
            Ok(config) => Ok(LoadOutcome {
                config,
                source: LoadSource::Primary,
                warning: None,
                writable: true,
            }),
            Err(ConfigError::NewerSchema { found, supported }) => Ok(LoadOutcome {
                config: Config::default(),
                source: LoadSource::NewerReadOnly,
                warning: Some(format!(
                    "config schema {found} is newer than supported schema {supported}; file preserved read-only"
                )),
                writable: false,
            }),
            Err(primary_error) => {
                let backup = fs::read_to_string(&self.backup_path);
                if let Ok(backup) = backup
                    && let Ok(config) = decode_and_migrate(&backup)
                {
                    return Ok(LoadOutcome {
                        config,
                        source: LoadSource::Backup,
                        warning: Some(format!(
                            "primary config was preserved after error; recovered backup: {primary_error}"
                        )),
                        writable: true,
                    });
                }
                Ok(LoadOutcome {
                    config: Config::default(),
                    source: LoadSource::SafeDefaults,
                    warning: Some(format!(
                        "primary config was preserved after error; using safe defaults: {primary_error}"
                    )),
                    writable: true,
                })
            }
        }
    }

    /// Durably replaces the authoritative config after complete validation.
    ///
    /// A valid previous primary becomes the backup. On the first save, the new
    /// candidate is also established as the recovery copy.
    ///
    /// # Errors
    ///
    /// Returns validation, serialization, permissions, write, rename, or sync failures.
    pub fn save(&self, candidate: &Config) -> Result<(), ConfigError> {
        let canonical = candidate.to_canonical_toml()?;
        if let Ok(existing) = fs::read_to_string(&self.path)
            && let Err(error @ ConfigError::NewerSchema { .. }) = decode_and_migrate(&existing)
        {
            return Err(error);
        }
        let parent = self.path.parent().ok_or_else(|| ConfigError::Io {
            path: self.path.clone(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "config path has no parent"),
        })?;
        fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;

        let backup_bytes = fs::read_to_string(&self.path)
            .ok()
            .filter(|existing| decode_and_migrate(existing).is_ok())
            .unwrap_or_else(|| canonical.clone());
        atomic_write(&self.backup_path, backup_bytes.as_bytes())?;
        atomic_write(&self.path, canonical.as_bytes())?;
        sync_directory(parent)?;
        Ok(())
    }
}

/// Parses current or supported legacy config into schema v1.
///
/// # Errors
///
/// Returns parse, migration, newer-schema, or validation failures.
pub fn decode_and_migrate(input: &str) -> Result<Config, ConfigError> {
    let document: toml::Value = toml::from_str(input)?;
    let version = document
        .get("schema_version")
        .and_then(toml::Value::as_integer)
        .and_then(|version| u64::try_from(version).ok())
        .ok_or(ConfigError::InvalidSchemaVersion)?;
    if version > u64::from(CONFIG_SCHEMA_VERSION) {
        return Err(ConfigError::NewerSchema {
            found: version,
            supported: CONFIG_SCHEMA_VERSION,
        });
    }
    let config = if version == 0 {
        let legacy: LegacyConfigV0 = toml::from_str(input)?;
        legacy.migrate()
    } else {
        toml::from_str(input)?
    };
    config.validate()?;
    Ok(config)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyConfigV0 {
    #[serde(rename = "schema_version")]
    _schema_version: u32,
    active: bool,
    input_node: String,
    suppression_enabled: bool,
    strength: f64,
}

impl LegacyConfigV0 {
    fn migrate(self) -> Config {
        let selected = !self.input_node.trim().is_empty();
        Config {
            schema_version: CONFIG_SCHEMA_VERSION,
            active: self.active,
            input: InputConfig {
                mode: if selected {
                    InputMode::Selected
                } else {
                    InputMode::FollowDefault
                },
                stable_id: self.input_node,
                ..InputConfig::default()
            },
            suppression: SuppressionConfig {
                enabled: self.suppression_enabled,
                strength: self.strength,
                ..SuppressionConfig::default()
            },
            ..Config::default()
        }
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let parent = path.parent().ok_or_else(|| ConfigError::Io {
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"),
    })?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    for attempt in 0_u8..16 {
        let temporary = parent.join(format!(".{file_name}.tmp-{}-{attempt}", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temporary) {
            Ok(mut file) => {
                let result = (|| {
                    file.write_all(bytes)?;
                    file.flush()?;
                    file.sync_all()?;
                    fs::rename(&temporary, path)?;
                    Ok::<(), io::Error>(())
                })();
                if let Err(source) = result {
                    let _ = fs::remove_file(&temporary);
                    return Err(io_error(path, source));
                }
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(io_error(&temporary, source)),
        }
    }
    Err(io_error(
        path,
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "temporary name attempts exhausted",
        ),
    ))
}

fn sync_directory(path: &Path) -> Result<(), ConfigError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(path, source))
}

fn io_error(path: &Path, source: io::Error) -> ConfigError {
    ConfigError::Io {
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, fs, time::SystemTime};

    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn arbitrary_config_documents_never_panic_or_escape_validation(input in any::<String>()) {
            if let Ok(config) = decode_and_migrate(&input) {
                prop_assert_eq!(config.schema_version, CONFIG_SCHEMA_VERSION);
                prop_assert!(config.validate().is_ok());
            }
        }

        #[test]
        fn legacy_migration_preserves_valid_finite_strength(
            active in any::<bool>(),
            enabled in any::<bool>(),
            selected in any::<bool>(),
            strength in 0.0_f64..=1.0,
        ) {
            let input_node = if selected { "alsa_input.fuzz" } else { "" };
            let legacy = format!(
                "schema_version = 0\nactive = {active}\ninput_node = '{input_node}'\n\
                 suppression_enabled = {enabled}\nstrength = {strength}\n"
            );
            let migrated = decode_and_migrate(&legacy)
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            prop_assert_eq!(migrated.active, active);
            prop_assert_eq!(migrated.suppression.enabled, enabled);
            prop_assert_eq!(migrated.input.mode == InputMode::Selected, selected);
            prop_assert!((migrated.suppression.strength - strength).abs() <= f64::EPSILON);
        }
    }

    fn temporary_store(test: &str) -> Result<(PathBuf, ConfigStore), Box<dyn Error>> {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos();
        let root = env::temp_dir().join(format!("noire-{test}-{}-{nonce}", std::process::id()));
        let store = ConfigStore::new(root.join("noire/config.toml"));
        Ok((root, store))
    }

    #[test]
    fn defaults_round_trip_as_canonical_schema_one() -> Result<(), Box<dyn Error>> {
        let config = Config::default();
        let text = config.to_canonical_toml()?;
        assert_eq!(decode_and_migrate(&text)?, config);
        assert_eq!(
            decode_and_migrate(include_str!("../../../data/config/config-v1.toml"))?,
            config
        );
        assert!(text.starts_with("schema_version = 1\nactive = false\n"));
        Ok(())
    }

    #[test]
    fn validates_full_paths_and_channel_indices() {
        let invalid = "schema_version = 1\nactive = false\nlaunch_at_login = false\n\
            [input]\nmode = 'selected'\nstable_id = ''\nchannel = 'index:3'\nfallback_to_default = false\n\
            [output]\nlatency_profile = 'low'\n\
            [suppression]\nenabled = true\nstrength = 1.0\nfail_mode = 'closed'\n\
            [diagnostics]\nlog_level = 'info'\nmetering = true\n";
        let result = decode_and_migrate(invalid);
        assert!(result.is_err(), "selected mode must require ID");
        if let Err(error) = result {
            assert!(error.to_string().contains("input.stable_id"));
        }
    }

    #[test]
    fn migrates_supported_v0_without_losing_intent() -> Result<(), Box<dyn Error>> {
        let legacy = "schema_version = 0\nactive = true\ninput_node = 'alsa_input.usb'\n\
            suppression_enabled = false\nstrength = 0.4\n";
        let migrated = decode_and_migrate(legacy)?;
        assert!(migrated.active);
        assert_eq!(migrated.input.mode, InputMode::Selected);
        assert_eq!(migrated.input.stable_id, "alsa_input.usb");
        assert!(!migrated.suppression.enabled);
        assert!((migrated.suppression.strength - 0.4).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn atomic_save_retains_backup_and_mode() -> Result<(), Box<dyn Error>> {
        let (root, store) = temporary_store("atomic")?;
        store.save(&Config::default())?;
        let mut changed = Config::default();
        changed.suppression.strength = 0.25;
        store.save(&changed)?;
        assert_eq!(
            decode_and_migrate(&fs::read_to_string(store.path())?)?,
            changed
        );
        assert_eq!(
            decode_and_migrate(&fs::read_to_string(store.backup_path())?)?,
            Config::default()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(store.path())?.permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn invalid_candidate_and_stale_temp_leave_primary_unchanged() -> Result<(), Box<dyn Error>> {
        let (root, store) = temporary_store("invalid-save")?;
        store.save(&Config::default())?;
        let before = fs::read(store.path())?;
        let parent = store.path().parent().ok_or("parent")?;
        let pid = std::process::id();
        let stale = parent.join(format!(".config.toml.tmp-{pid}-0"));
        fs::write(stale, "interrupted")?;
        let mut invalid = Config::default();
        invalid.suppression.strength = 2.0;
        assert!(matches!(
            store.save(&invalid),
            Err(ConfigError::Validation(_))
        ));
        assert_eq!(fs::read(store.path())?, before);
        let mut valid = Config::default();
        valid.suppression.strength = 0.75;
        store.save(&valid)?;
        assert_eq!(store.load()?.config, valid);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn malformed_primary_recovers_backup_without_rewriting_primary() -> Result<(), Box<dyn Error>> {
        let (root, store) = temporary_store("recover")?;
        let config = Config::default();
        store.save(&config)?;
        fs::write(store.path(), "not = [valid")?;
        let before = fs::read(store.path())?;
        let outcome = store.load()?;
        assert_eq!(outcome.source, LoadSource::Backup);
        assert_eq!(outcome.config, config);
        assert_eq!(fs::read(store.path())?, before);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn newer_schema_is_read_only_and_byte_preserved() -> Result<(), Box<dyn Error>> {
        let (root, store) = temporary_store("newer")?;
        fs::create_dir_all(store.path().parent().ok_or("parent")?)?;
        let newer = b"schema_version = 99\nfuture = true\n";
        fs::write(store.path(), newer)?;
        let outcome = store.load()?;
        assert_eq!(outcome.source, LoadSource::NewerReadOnly);
        assert!(!outcome.writable);
        assert!(matches!(
            store.save(&Config::default()),
            Err(ConfigError::NewerSchema { found: 99, .. })
        ));
        assert_eq!(fs::read(store.path())?, newer);
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
