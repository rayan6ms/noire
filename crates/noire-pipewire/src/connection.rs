//! Thread-affine connection to the `PipeWire` core and registry.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    time::{Duration, Instant},
};

use pipewire::{
    context::ContextRc,
    core::{self, CoreRc},
    loop_::Timeout,
    main_loop::MainLoopRc,
    metadata::{Metadata, MetadataListener},
    node::{Node, NodeListener},
    registry::{self, RegistryRc},
    types::ObjectType,
};

use crate::{NodeDescriptor, NodeProperties, RegistryMonitor, RegistrySnapshot};

const DEFAULT_METADATA_NAME: &str = "default";
const DEFAULT_AUDIO_SOURCE_KEY: &str = "default.audio.source";

/// Compact control-plane description of a fatal `PipeWire` core error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoreFailure {
    /// `PipeWire` object that reported the error.
    pub object_id: u32,
    /// Native PipeWire/SPA result code.
    pub result: i32,
    /// Human-readable server message captured outside process callbacks.
    pub message: String,
}

#[derive(Debug, Default)]
struct ConnectionState {
    runtime_version: Option<String>,
    failure: Option<CoreFailure>,
}

/// Owns the `PipeWire` main loop, context, core, registry, and core listener.
///
/// This value is intentionally neither `Send` nor `Sync`: it must be created,
/// dispatched, and dropped on the PipeWire-owning thread.
pub struct PipewireConnection {
    main_loop: MainLoopRc,
    core: CoreRc,
    _registry: RegistryRc,
    clock_origin: Instant,
    registry_monitor: Rc<RefCell<RegistryMonitor>>,
    _node_bindings: Rc<RefCell<BTreeMap<u32, NodeBinding>>>,
    _metadata_bindings: Rc<RefCell<BTreeMap<u32, MetadataBinding>>>,
    state: Rc<RefCell<ConnectionState>>,
    _core_listener: core::Listener,
    _registry_listener: registry::Listener,
}

struct NodeBinding {
    _listener: NodeListener,
    _node: Node,
}

struct MetadataBinding {
    _listener: MetadataListener,
    _metadata: Metadata,
}

impl PipewireConnection {
    /// Connects to the default `PipeWire` remote and binds its registry.
    ///
    /// # Errors
    ///
    /// Returns the native binding error when the loop, context, core, or
    /// registry cannot be created.
    pub fn connect_default() -> Result<Self, pipewire::Error> {
        pipewire::init();
        let main_loop = MainLoopRc::new(None)?;
        let context = ContextRc::new(&main_loop, None)?;
        let core = context.connect_rc(None)?;
        let registry = core.get_registry_rc()?;
        let state = Rc::new(RefCell::new(ConnectionState::default()));
        let clock_origin = Instant::now();
        let registry_monitor = Rc::new(RefCell::new(RegistryMonitor::new()));
        let node_bindings = Rc::new(RefCell::new(BTreeMap::new()));
        let metadata_bindings = Rc::new(RefCell::new(BTreeMap::new()));

        let info_state = Rc::clone(&state);
        let error_state = Rc::clone(&state);
        let core_listener = core
            .add_listener_local()
            .info(move |info| {
                info_state.borrow_mut().runtime_version = Some(info.version().to_owned());
            })
            .error(move |object_id, _sequence, result, message| {
                error_state.borrow_mut().failure = Some(CoreFailure {
                    object_id,
                    result,
                    message: message.to_owned(),
                });
            })
            .register();

        let registry_listener = register_registry_listener(
            &registry,
            &registry_monitor,
            &node_bindings,
            &metadata_bindings,
            clock_origin,
        );

        Ok(Self {
            main_loop,
            core,
            _registry: registry,
            clock_origin,
            registry_monitor,
            _node_bindings: node_bindings,
            _metadata_bindings: metadata_bindings,
            state,
            _core_listener: core_listener,
            _registry_listener: registry_listener,
        })
    }

    /// Dispatches one bounded main-loop iteration on the owning thread.
    #[must_use]
    pub fn dispatch_once(&self, timeout: Duration) -> i32 {
        self.main_loop
            .loop_()
            .iterate(Timeout::Finite(timeout.min(Duration::from_secs(1))))
    }

    /// Runs the owning main loop until [`Self::quit`] is requested.
    pub fn run(&self) {
        self.main_loop.run();
    }

    /// Requests that a running main loop return.
    pub fn quit(&self) {
        self.main_loop.quit();
    }

    /// Returns the server runtime version after its core-info event arrives.
    #[must_use]
    pub fn runtime_version(&self) -> Option<String> {
        self.state.borrow().runtime_version.clone()
    }

    /// Removes and returns the latest fatal core error.
    #[must_use]
    pub fn take_failure(&self) -> Option<CoreFailure> {
        self.state.borrow_mut().failure.take()
    }

    /// Requests a round trip so callers can identify an initial snapshot
    /// boundary through subsequently dispatched events.
    ///
    /// # Errors
    ///
    /// Returns the native binding error if `PipeWire` rejects the sync request.
    pub fn request_roundtrip(&self) -> Result<(), pipewire::Error> {
        self.core.sync(0)?;
        Ok(())
    }

    /// Publishes a coalesced immutable device snapshot when its debounce expires.
    #[must_use]
    pub fn registry_snapshot_if_due(&self) -> Option<RegistrySnapshot> {
        self.registry_monitor
            .borrow_mut()
            .publish_if_due(elapsed_millis(self.clock_origin))
    }

    /// Publishes the current device snapshot at an explicit round-trip boundary.
    #[must_use]
    pub fn registry_snapshot_now(&self) -> RegistrySnapshot {
        self.registry_monitor.borrow_mut().publish_now()
    }
}

fn register_registry_listener(
    registry: &RegistryRc,
    monitor: &Rc<RefCell<RegistryMonitor>>,
    nodes: &Rc<RefCell<BTreeMap<u32, NodeBinding>>>,
    metadata: &Rc<RefCell<BTreeMap<u32, MetadataBinding>>>,
    clock_origin: Instant,
) -> registry::Listener {
    let add_registry = registry.downgrade();
    let add_monitor = Rc::clone(monitor);
    let add_nodes = Rc::clone(nodes);
    let add_metadata = Rc::clone(metadata);
    let remove_monitor = Rc::clone(monitor);
    let remove_nodes = Rc::clone(nodes);
    let remove_metadata = Rc::clone(metadata);

    registry
        .add_listener_local()
        .global(move |global| {
            let Some(registry) = add_registry.upgrade() else {
                return;
            };
            match global.type_ {
                ObjectType::Node => {
                    register_node(global, &registry, &add_monitor, &add_nodes, clock_origin);
                }
                ObjectType::Metadata if is_default_metadata(global) => {
                    register_metadata(global, &registry, &add_monitor, &add_metadata, clock_origin);
                }
                _ => {}
            }
        })
        .global_remove(move |global_id| {
            remove_nodes.borrow_mut().remove(&global_id);
            remove_metadata.borrow_mut().remove(&global_id);
            remove_monitor
                .borrow_mut()
                .remove(global_id, elapsed_millis(clock_origin));
        })
        .register()
}

fn register_node(
    global: &registry::GlobalObject<&libspa::utils::dict::DictRef>,
    registry: &RegistryRc,
    monitor: &Rc<RefCell<RegistryMonitor>>,
    bindings: &Rc<RefCell<BTreeMap<u32, NodeBinding>>>,
    clock_origin: Instant,
) {
    if let Some(properties) = global.props.as_ref() {
        update_node(monitor, global.id, properties, clock_origin);
    }
    let Ok(node) = registry.bind::<Node, _>(global) else {
        return;
    };
    let info_monitor = Rc::clone(monitor);
    let global_id = global.id;
    let listener = node
        .add_listener_local()
        .info(move |info| {
            if let Some(properties) = info.props() {
                update_node(&info_monitor, global_id, properties, clock_origin);
            }
        })
        .register();
    bindings.borrow_mut().insert(
        global.id,
        NodeBinding {
            _listener: listener,
            _node: node,
        },
    );
}

fn update_node(
    monitor: &Rc<RefCell<RegistryMonitor>>,
    global_id: u32,
    properties: &libspa::utils::dict::DictRef,
    clock_origin: Instant,
) {
    let properties = owned_properties(properties);
    if let Some(descriptor) = NodeDescriptor::from_properties(global_id, &properties) {
        monitor
            .borrow_mut()
            .upsert(descriptor, elapsed_millis(clock_origin));
    }
}

fn is_default_metadata(global: &registry::GlobalObject<&libspa::utils::dict::DictRef>) -> bool {
    global
        .props
        .as_ref()
        .and_then(|properties| properties.get("metadata.name"))
        == Some(DEFAULT_METADATA_NAME)
}

fn register_metadata(
    global: &registry::GlobalObject<&libspa::utils::dict::DictRef>,
    registry: &RegistryRc,
    monitor: &Rc<RefCell<RegistryMonitor>>,
    bindings: &Rc<RefCell<BTreeMap<u32, MetadataBinding>>>,
    clock_origin: Instant,
) {
    let Ok(metadata) = registry.bind::<Metadata, _>(global) else {
        return;
    };
    let default_monitor = Rc::clone(monitor);
    let listener = metadata
        .add_listener_local()
        .property(move |_subject, key, _value_type, value| {
            if key == Some(DEFAULT_AUDIO_SOURCE_KEY) {
                let node_name = value.and_then(crate::registry::parse_default_node_name);
                default_monitor
                    .borrow_mut()
                    .set_default_node_name(node_name, elapsed_millis(clock_origin));
            }
            0
        })
        .register();
    bindings.borrow_mut().insert(
        global.id,
        MetadataBinding {
            _listener: listener,
            _metadata: metadata,
        },
    );
}

fn owned_properties(properties: &libspa::utils::dict::DictRef) -> NodeProperties {
    NodeProperties::new(properties.iter())
}

fn elapsed_millis(origin: Instant) -> u64 {
    u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX)
}
