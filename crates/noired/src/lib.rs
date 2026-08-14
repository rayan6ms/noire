//! Daemon lifecycle, transactional control state, and runtime adapters.

#![forbid(unsafe_code)]

mod control;
mod diagnostics;
#[cfg(all(feature = "runtime", feature = "pipewire-backend"))]
mod native;
mod recovery;
#[cfg(feature = "runtime")]
mod service;
#[cfg(feature = "runtime")]
mod systemd;

pub use control::{
    AudioEngine, ControlError, Daemon, EngineError, EngineObservation, LifecycleState,
    NullAudioEngine,
};
pub use diagnostics::EventRateLimiter;
#[cfg(all(feature = "runtime", feature = "pipewire-backend"))]
pub use native::NativeAudioEngine;
pub use recovery::{
    INITIAL_BACKOFF_MS, MAX_BACKOFF_MS, RecoveryAttempt, RecoveryController, RecoveryFault,
    RecoveryPhase, RecoveryStats,
};
#[cfg(feature = "runtime")]
pub use service::{NoireService, claim_name, register_and_claim, register_service};
#[cfg(feature = "runtime")]
pub use systemd::{LaunchManager, LaunchManagerError, SystemdUserManager};

/// systemd user unit controlled through its D-Bus manager.
pub const USER_UNIT_NAME: &str = "noire.service";
