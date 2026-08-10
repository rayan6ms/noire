//! Thread-affine connection to the `PipeWire` core and registry.

use std::{cell::RefCell, rc::Rc, time::Duration};

use pipewire::{
    context::ContextRc,
    core::{self, CoreRc},
    loop_::Timeout,
    main_loop::MainLoopRc,
    registry::RegistryRc,
};

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
    state: Rc<RefCell<ConnectionState>>,
    _core_listener: core::Listener,
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

        Ok(Self {
            main_loop,
            core,
            _registry: registry,
            state,
            _core_listener: core_listener,
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
}
