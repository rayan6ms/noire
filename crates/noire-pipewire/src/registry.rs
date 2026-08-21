//! Immutable input descriptors and stable `PipeWire` selectors.

use std::{collections::BTreeMap, sync::Arc};

/// Stable `PipeWire` node name reserved for Noire's future virtual source.
pub const RESERVED_NODE_NAME: &str = "io.github.rayan6ms.Noire.Microphone";

const MEDIA_CLASS_SOURCE: &str = "Audio/Source";

/// Control-thread registry debounce required by the architecture contract.
pub const REGISTRY_COALESCE_MILLIS: u64 = 50;

/// Availability reported for a capture node.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DeviceAvailability {
    /// The node is available for capture.
    Available,
    /// The node is known to be unavailable.
    Unavailable,
    /// The registry did not expose a usable availability property.
    #[default]
    Unknown,
}

/// One advertised raw-audio layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdvertisedFormat {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Interleaved channel count.
    pub channels: u16,
    /// Ordered SPA channel-position names.
    pub positions: Arc<[String]>,
}

/// Owned property input used at the PipeWire/control-plane boundary.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NodeProperties {
    values: BTreeMap<String, String>,
}

impl NodeProperties {
    /// Builds an owned property set from arbitrary key/value pairs.
    #[must_use]
    pub fn new<I, K, V>(values: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            values: values
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        }
    }

    /// Returns a property value by its `PipeWire` key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
}

/// Immutable capture-node information copied from a registry event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeDescriptor {
    /// Transient `PipeWire` registry global ID. Never persist this value.
    pub global_id: u32,
    /// `PipeWire` object serial when reported.
    pub object_serial: Option<u64>,
    /// Stable node name used as a capture target.
    pub node_name: String,
    /// Human-readable node description.
    pub description: Option<String>,
    /// Short node nickname.
    pub nick: Option<String>,
    /// Media class, normally `Audio/Source` for candidates.
    pub media_class: Option<String>,
    /// Optional media role.
    pub media_role: Option<String>,
    /// Whether `PipeWire` marks the node virtual.
    pub virtual_node: bool,
    /// Owning `PipeWire` device global ID when reported.
    pub device_id: Option<u32>,
    /// Stable device name when reported.
    pub device_name: Option<String>,
    /// Stable hardware/device serial when reported.
    pub device_serial: Option<String>,
    /// Backend API, such as ALSA or `BlueZ`.
    pub device_api: Option<String>,
    /// Advertised raw-audio layouts parsed from registry properties or params.
    pub formats: Arc<[AdvertisedFormat]>,
    /// Current availability state.
    pub availability: DeviceAvailability,
    /// Whether this node is the session-manager default input.
    pub is_default: bool,
    /// Stable, deduplicated label assigned by [`RegistrySnapshot`].
    pub label: String,
}

impl NodeDescriptor {
    /// Parses one node global. Missing `node.name` produces `None`.
    #[must_use]
    pub fn from_properties(global_id: u32, properties: &NodeProperties) -> Option<Self> {
        let node_name = nonempty(properties.get("node.name"))?.to_owned();
        let positions = properties
            .get("audio.position")
            .map(parse_positions)
            .unwrap_or_default();
        let formats = match (
            parse_property::<u32>(properties, "audio.rate"),
            parse_property::<u16>(properties, "audio.channels"),
        ) {
            (Some(sample_rate), Some(channels)) if sample_rate != 0 && channels != 0 => {
                vec![AdvertisedFormat {
                    sample_rate,
                    channels,
                    positions: positions.into(),
                }]
                .into()
            }
            _ => Arc::from([]),
        };
        let description = owned_nonempty(properties.get("node.description"));
        let nick = owned_nonempty(properties.get("node.nick"));
        let label = description
            .as_deref()
            .or(nick.as_deref())
            .unwrap_or(&node_name)
            .to_owned();

        Some(Self {
            global_id,
            object_serial: parse_property(properties, "object.serial"),
            node_name,
            description,
            nick,
            media_class: owned_nonempty(properties.get("media.class")),
            media_role: owned_nonempty(properties.get("media.role")),
            virtual_node: parse_bool(properties.get("node.virtual")),
            device_id: parse_property(properties, "device.id"),
            device_name: owned_nonempty(properties.get("device.name")),
            device_serial: owned_nonempty(properties.get("device.serial")),
            device_api: owned_nonempty(properties.get("device.api")),
            formats,
            availability: parse_availability(properties),
            is_default: false,
            label,
        })
    }

    /// Returns whether the node is an eligible physical microphone candidate.
    #[must_use]
    pub fn is_candidate(&self) -> bool {
        self.media_class.as_deref() == Some(MEDIA_CLASS_SOURCE)
            && !self.virtual_node
            && self.node_name != RESERVED_NODE_NAME
            && !is_monitor_name(&self.node_name)
            && !self
                .description
                .as_deref()
                .is_some_and(is_monitor_description)
            && self.availability != DeviceAvailability::Unavailable
    }

    /// Generates the strongest stable selector supported by this descriptor.
    #[must_use]
    pub fn selector(&self) -> DeviceSelector {
        DeviceSelector {
            node_name: self.node_name.clone(),
            device_serial: self.device_serial.clone(),
            device_name: self.device_name.clone(),
        }
    }
}

/// Persistable input identity ordered from strongest to weakest match fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceSelector {
    /// Exact stable `PipeWire` node name.
    pub node_name: String,
    /// Hardware/device serial, preferred when present.
    pub device_serial: Option<String>,
    /// Device name used when a serial is unavailable.
    pub device_name: Option<String>,
}

impl DeviceSelector {
    /// Returns whether this selector identifies `candidate` without consulting a
    /// transient `PipeWire` global ID.
    #[must_use]
    pub fn matches(&self, candidate: &NodeDescriptor) -> bool {
        if self.node_name != candidate.node_name {
            return false;
        }
        if let Some(serial) = self.device_serial.as_deref() {
            return candidate.device_serial.as_deref() == Some(serial);
        }
        if let Some(device_name) = self.device_name.as_deref() {
            return candidate.device_name.as_deref() == Some(device_name);
        }
        true
    }
}

/// Immutable, stably ordered view of eligible input candidates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrySnapshot {
    revision: u64,
    node_names: Arc<[String]>,
    candidates: Arc<[NodeDescriptor]>,
}

/// Explicit policy for resolving one immutable registry snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SelectionPolicy {
    /// Persisted stable selector, when the user chose a specific input.
    pub selector: Option<DeviceSelector>,
    /// Follow the session-manager default when no specific selector exists.
    pub follow_default: bool,
    /// Permit default fallback when a persisted specific selector is missing.
    pub fallback_to_default: bool,
}

/// Visible reason no capture node could be selected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputUnavailable {
    /// The persisted input is absent and fallback was not authorized.
    SelectedInputMissing,
    /// Default-following was requested but no eligible default exists.
    DefaultInputMissing,
    /// No selector/default policy was configured.
    NotConfigured,
}

/// Result of applying stable selection policy to one registry snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputResolution {
    /// An eligible node was resolved.
    Selected(Box<NodeDescriptor>),
    /// No node was resolved, with a public control-plane reason.
    Unavailable(InputUnavailable),
}

/// Mutable control-thread registry builder with a fixed 50 ms publish debounce.
///
/// Process callbacks never access this collection. Consumers receive cloned,
/// immutable [`RegistrySnapshot`] values only after the coalescing boundary.
#[derive(Debug, Default)]
pub struct RegistryMonitor {
    nodes: BTreeMap<u32, NodeDescriptor>,
    default_node_name: Option<String>,
    dirty_since_millis: Option<u64>,
    revision: u64,
}

impl RegistryMonitor {
    /// Creates an empty monitor.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            nodes: BTreeMap::new(),
            default_node_name: None,
            dirty_since_millis: None,
            revision: 0,
        }
    }

    /// Adds or replaces one node and starts the debounce window if it changed.
    pub fn upsert(&mut self, node: NodeDescriptor, now_millis: u64) {
        let changed = self.nodes.get(&node.global_id) != Some(&node);
        if changed {
            self.nodes.insert(node.global_id, node);
            self.mark_dirty(now_millis);
        }
    }

    /// Removes a transient global ID and starts the debounce window if present.
    pub fn remove(&mut self, global_id: u32, now_millis: u64) {
        if self.nodes.remove(&global_id).is_some() {
            self.mark_dirty(now_millis);
        }
    }

    /// Updates the session-manager default by stable node name.
    pub fn set_default_node_name(&mut self, node_name: Option<String>, now_millis: u64) {
        if self.default_node_name != node_name {
            self.default_node_name = node_name;
            self.mark_dirty(now_millis);
        }
    }

    /// Publishes a new immutable snapshot once the 50 ms window has elapsed.
    pub fn publish_if_due(&mut self, now_millis: u64) -> Option<RegistrySnapshot> {
        let dirty_since = self.dirty_since_millis?;
        if now_millis.saturating_sub(dirty_since) < REGISTRY_COALESCE_MILLIS {
            return None;
        }
        Some(self.publish_now())
    }

    /// Publishes immediately, used at the initial `PipeWire` round-trip boundary.
    #[must_use]
    pub fn publish_now(&mut self) -> RegistrySnapshot {
        self.dirty_since_millis = None;
        self.revision = self.revision.saturating_add(1);
        let mut nodes: Vec<_> = self.nodes.values().cloned().collect();
        for node in &mut nodes {
            node.is_default = self.default_node_name.as_deref() == Some(&node.node_name);
        }
        RegistrySnapshot::new(self.revision, nodes)
    }

    /// Returns the number of raw node globals currently retained.
    #[must_use]
    pub fn raw_node_count(&self) -> usize {
        self.nodes.len()
    }

    fn mark_dirty(&mut self, now_millis: u64) {
        self.dirty_since_millis.get_or_insert(now_millis);
    }
}

impl RegistrySnapshot {
    /// Filters, sorts, and stably deduplicates labels for one registry view.
    #[must_use]
    pub fn new(revision: u64, nodes: impl IntoIterator<Item = NodeDescriptor>) -> Self {
        let nodes: Vec<_> = nodes.into_iter().collect();
        let node_names = nodes.iter().map(|node| node.node_name.clone()).collect();
        let mut candidates: Vec<_> = nodes
            .into_iter()
            .filter(NodeDescriptor::is_candidate)
            .collect();
        candidates.sort_by(|left, right| {
            left.label
                .to_lowercase()
                .cmp(&right.label.to_lowercase())
                .then_with(|| left.node_name.cmp(&right.node_name))
        });
        deduplicate_labels(&mut candidates);
        Self {
            revision,
            node_names,
            candidates: candidates.into(),
        }
    }

    /// Monotonic control-plane snapshot revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Eligible physical inputs in stable label/name order.
    #[must_use]
    pub fn candidates(&self) -> &[NodeDescriptor] {
        &self.candidates
    }

    /// Counts raw node globals with one stable node name, including virtual nodes.
    #[must_use]
    pub fn node_name_occurrences(&self, node_name: &str) -> usize {
        self.node_names
            .iter()
            .filter(|candidate| candidate.as_str() == node_name)
            .count()
    }

    /// Applies explicit selector/default policy without using global IDs.
    #[must_use]
    pub fn resolve(&self, policy: &SelectionPolicy) -> InputResolution {
        if let Some(selector) = policy.selector.as_ref() {
            if let Some(candidate) = self
                .candidates
                .iter()
                .find(|candidate| selector.matches(candidate))
            {
                return InputResolution::Selected(Box::new(candidate.clone()));
            }
            if !policy.fallback_to_default {
                return InputResolution::Unavailable(InputUnavailable::SelectedInputMissing);
            }
        } else if !policy.follow_default {
            return InputResolution::Unavailable(InputUnavailable::NotConfigured);
        }

        self.candidates
            .iter()
            .find(|candidate| candidate.is_default)
            .cloned()
            .map_or(
                InputResolution::Unavailable(InputUnavailable::DefaultInputMissing),
                |candidate| InputResolution::Selected(Box::new(candidate)),
            )
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn owned_nonempty(value: Option<&str>) -> Option<String> {
    nonempty(value).map(str::to_owned)
}

fn parse_property<T: std::str::FromStr>(properties: &NodeProperties, key: &str) -> Option<T> {
    properties.get(key)?.parse().ok()
}

fn parse_bool(value: Option<&str>) -> bool {
    value.is_some_and(|value| matches!(value, "1" | "true" | "yes" | "on"))
}

fn parse_availability(properties: &NodeProperties) -> DeviceAvailability {
    match properties.get("device.available") {
        Some("1" | "true" | "yes" | "available") => DeviceAvailability::Available,
        Some("0" | "false" | "no" | "unavailable") => DeviceAvailability::Unavailable,
        _ => DeviceAvailability::Unknown,
    }
}

fn parse_positions(value: &str) -> Vec<String> {
    value
        .trim_matches(['[', ']'])
        .split([',', ' '])
        .map(str::trim)
        .filter(|position| !position.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(any(feature = "pipewire-backend", test))]
pub(crate) fn parse_default_node_name(value: &str) -> Option<String> {
    let name_key = value.find("\"name\"")?;
    let after_key = value.get(name_key + "\"name\"".len()..)?;
    let after_colon = after_key.split_once(':')?.1.trim_start();
    let quoted = after_colon.strip_prefix('"')?;
    let end = quoted.find('"')?;
    nonempty(quoted.get(..end)).map(str::to_owned)
}

fn is_monitor_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".monitor") || lower.contains(".monitor.")
}

fn is_monitor_description(description: &str) -> bool {
    description.to_ascii_lowercase().starts_with("monitor of ")
}

fn deduplicate_labels(candidates: &mut [NodeDescriptor]) {
    let mut counts = BTreeMap::<String, usize>::new();
    for candidate in candidates.iter() {
        *counts.entry(candidate.label.clone()).or_default() += 1;
    }
    for candidate in candidates.iter_mut() {
        if counts.get(&candidate.label).copied().unwrap_or_default() > 1 {
            candidate.label = format!("{} ({})", candidate.label, candidate.node_name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DeviceAvailability, InputResolution, InputUnavailable, NodeDescriptor, NodeProperties,
        REGISTRY_COALESCE_MILLIS, RESERVED_NODE_NAME, RegistryMonitor, RegistrySnapshot,
        SelectionPolicy,
    };

    fn physical(global_id: u32, name: &str, description: &str) -> Option<NodeDescriptor> {
        NodeDescriptor::from_properties(
            global_id,
            &NodeProperties::new([
                ("node.name", name),
                ("node.description", description),
                ("media.class", "Audio/Source"),
                ("device.serial", "usb-123"),
                ("device.name", "alsa_card.usb"),
                ("device.api", "alsa"),
                ("object.serial", "9001"),
                ("audio.rate", "48000"),
                ("audio.channels", "1"),
                ("audio.position", "[ MONO ]"),
                ("device.available", "yes"),
            ]),
        )
    }

    #[test]
    fn parses_identity_format_and_stable_selector() -> Result<(), &'static str> {
        let first = physical(7, "alsa_input.usb", "USB Microphone").ok_or("missing fixture")?;
        let second = physical(99, "alsa_input.usb", "USB Microphone").ok_or("missing fixture")?;

        assert_eq!(first.object_serial, Some(9001));
        assert_eq!(first.availability, DeviceAvailability::Available);
        assert_eq!(first.formats[0].sample_rate, 48_000);
        assert_eq!(&*first.formats[0].positions, &["MONO"]);
        assert_eq!(first.selector(), second.selector());
        assert!(first.selector().matches(&second));
        Ok(())
    }

    #[test]
    fn excludes_monitor_virtual_noire_unavailable_and_non_source_nodes() -> Result<(), &'static str>
    {
        let mut virtual_node = physical(2, "virtual.mic", "Virtual").ok_or("missing fixture")?;
        virtual_node.virtual_node = true;
        let mut noire =
            physical(3, RESERVED_NODE_NAME, "Noire Microphone ☾").ok_or("missing fixture")?;
        noire.virtual_node = false;
        let mut unavailable = physical(4, "alsa_input.gone", "Gone").ok_or("missing fixture")?;
        unavailable.availability = DeviceAvailability::Unavailable;
        let mut sink = physical(5, "alsa_output.speakers", "Speakers").ok_or("missing fixture")?;
        sink.media_class = Some("Audio/Sink".to_owned());

        let snapshot = RegistrySnapshot::new(
            1,
            [
                physical(1, "alsa_output.speakers.monitor", "Monitor of Speakers")
                    .ok_or("missing fixture")?,
                virtual_node,
                noire,
                unavailable,
                sink,
                physical(6, "alsa_input.real", "Real Microphone").ok_or("missing fixture")?,
            ],
        );

        assert_eq!(snapshot.candidates().len(), 1);
        assert_eq!(snapshot.candidates()[0].node_name, "alsa_input.real");
        assert_eq!(snapshot.node_name_occurrences(RESERVED_NODE_NAME), 1);
        assert_eq!(snapshot.node_name_occurrences("missing.node"), 0);
        Ok(())
    }

    #[test]
    fn duplicate_human_labels_receive_stable_node_qualifiers() -> Result<(), &'static str> {
        let snapshot = RegistrySnapshot::new(
            4,
            [
                physical(2, "alsa_input.rear", "Microphone").ok_or("missing fixture")?,
                physical(1, "alsa_input.front", "Microphone").ok_or("missing fixture")?,
            ],
        );

        assert_eq!(snapshot.revision(), 4);
        assert_eq!(
            snapshot.candidates()[0].label,
            "Microphone (alsa_input.front)"
        );
        assert_eq!(
            snapshot.candidates()[1].label,
            "Microphone (alsa_input.rear)"
        );
        Ok(())
    }

    #[test]
    fn malformed_or_missing_properties_do_not_panic() -> Result<(), &'static str> {
        assert!(NodeDescriptor::from_properties(1, &NodeProperties::default()).is_none());
        let descriptor = NodeDescriptor::from_properties(
            2,
            &NodeProperties::new([
                ("node.name", "source"),
                ("media.class", "Audio/Source"),
                ("audio.rate", "not-a-number"),
                ("audio.channels", "0"),
            ]),
        )
        .ok_or("node name should parse")?;
        assert!(descriptor.formats.is_empty());
        assert_eq!(descriptor.availability, DeviceAvailability::Unknown);
        Ok(())
    }

    #[test]
    fn coalesces_add_change_remove_and_default_updates() -> Result<(), &'static str> {
        let mut monitor = RegistryMonitor::new();
        monitor.upsert(
            physical(10, "alsa_input.first", "First").ok_or("missing fixture")?,
            1_000,
        );
        monitor.upsert(
            physical(11, "alsa_input.second", "Second").ok_or("missing fixture")?,
            1_010,
        );
        monitor.set_default_node_name(Some("alsa_input.second".to_owned()), 1_020);
        monitor.remove(10, 1_030);

        assert!(monitor.publish_if_due(1_049).is_none());
        let snapshot = monitor
            .publish_if_due(1_000 + REGISTRY_COALESCE_MILLIS)
            .ok_or("snapshot should be due")?;
        assert_eq!(snapshot.revision(), 1);
        assert_eq!(snapshot.candidates().len(), 1);
        assert!(snapshot.candidates()[0].is_default);
        assert!(monitor.publish_if_due(2_000).is_none());
        Ok(())
    }

    #[test]
    fn stable_selection_never_falls_back_without_permission() -> Result<(), &'static str> {
        let selected = physical(1, "alsa_input.selected", "Selected").ok_or("missing fixture")?;
        let selector = selected.selector();
        let mut default = physical(2, "alsa_input.default", "Default").ok_or("missing fixture")?;
        default.is_default = true;
        let snapshot = RegistrySnapshot::new(1, [default.clone()]);

        assert_eq!(
            snapshot.resolve(&SelectionPolicy {
                selector: Some(selector.clone()),
                follow_default: false,
                fallback_to_default: false,
            }),
            InputResolution::Unavailable(InputUnavailable::SelectedInputMissing)
        );
        assert_eq!(
            snapshot.resolve(&SelectionPolicy {
                selector: Some(selector),
                follow_default: false,
                fallback_to_default: true,
            }),
            InputResolution::Selected(Box::new(default))
        );
        Ok(())
    }

    #[test]
    fn reidentified_selected_node_resolves_by_stable_selector() -> Result<(), &'static str> {
        let original = physical(7, "alsa_input.usb", "USB").ok_or("missing fixture")?;
        let reidentified = physical(700, "alsa_input.usb", "USB").ok_or("missing fixture")?;
        let snapshot = RegistrySnapshot::new(2, [reidentified.clone()]);

        assert_eq!(
            snapshot.resolve(&SelectionPolicy {
                selector: Some(original.selector()),
                follow_default: false,
                fallback_to_default: false,
            }),
            InputResolution::Selected(Box::new(reidentified))
        );
        Ok(())
    }

    #[test]
    fn parses_default_metadata_without_accepting_malformed_values() {
        assert_eq!(
            super::parse_default_node_name("{ \"name\": \"alsa_input.usb\" }"),
            Some("alsa_input.usb".to_owned())
        );
        assert_eq!(super::parse_default_node_name("{ \"name\": \"\" }"), None);
        assert_eq!(super::parse_default_node_name("not-json"), None);
    }

    #[test]
    fn fifty_reidentified_hotplug_cycles_leave_registry_empty_and_selector_stable()
    -> Result<(), &'static str> {
        let mut monitor = RegistryMonitor::new();
        let mut expected_selector = None;

        for cycle in 0_u32..50 {
            let global_id = cycle.saturating_add(100);
            let now = u64::from(cycle).saturating_mul(200);
            let node = physical(global_id, "alsa_input.hotplug", "Hotplug Microphone")
                .ok_or("missing fixture")?;
            let selector = node.selector();
            if let Some(expected) = expected_selector.as_ref() {
                assert_eq!(&selector, expected);
            } else {
                expected_selector = Some(selector);
            }
            monitor.upsert(node, now);
            let added = monitor
                .publish_if_due(now.saturating_add(REGISTRY_COALESCE_MILLIS))
                .ok_or("add did not publish")?;
            assert_eq!(added.candidates().len(), 1);

            monitor.remove(global_id, now.saturating_add(REGISTRY_COALESCE_MILLIS + 1));
            let removed = monitor
                .publish_if_due(now.saturating_add((REGISTRY_COALESCE_MILLIS * 2) + 1))
                .ok_or("remove did not publish")?;
            assert!(removed.candidates().is_empty());
            assert_eq!(monitor.raw_node_count(), 0);
        }

        assert_eq!(monitor.publish_now().revision(), 101);
        Ok(())
    }
}
