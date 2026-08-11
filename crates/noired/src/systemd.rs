//! Per-user systemd manager adapter. No subprocess is ever spawned.

use std::{future::Future, pin::Pin};

use thiserror::Error;

use crate::USER_UNIT_NAME;

/// Launch-manager operation failure.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct LaunchManagerError {
    /// Stable machine code.
    pub code: &'static str,
    /// Sanitized explanation.
    pub message: String,
}

/// Fakeable asynchronous launch-at-login boundary.
pub trait LaunchManager: Send + Sync {
    /// Enables or disables the Noire user unit through the manager API.
    fn set_enabled<'a>(
        &'a self,
        enabled: bool,
    ) -> Pin<Box<dyn Future<Output = Result<(), LaunchManagerError>> + Send + 'a>>;
}

#[allow(missing_docs)]
#[zbus::proxy(
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1",
    interface = "org.freedesktop.systemd1.Manager"
)]
trait SystemdManager {
    fn enable_unit_files(
        &self,
        files: Vec<String>,
        runtime: bool,
        force: bool,
    ) -> zbus::Result<(bool, Vec<(String, String, String)>)>;

    fn disable_unit_files(
        &self,
        files: Vec<String>,
        runtime: bool,
    ) -> zbus::Result<Vec<(String, String, String)>>;

    fn reload(&self) -> zbus::Result<()>;
}

/// Production per-user `org.freedesktop.systemd1.Manager` client.
#[derive(Clone, Debug)]
pub struct SystemdUserManager {
    connection: zbus::Connection,
}

impl SystemdUserManager {
    /// Uses an existing session-bus connection.
    #[must_use]
    pub const fn new(connection: zbus::Connection) -> Self {
        Self { connection }
    }
}

impl LaunchManager for SystemdUserManager {
    fn set_enabled<'a>(
        &'a self,
        enabled: bool,
    ) -> Pin<Box<dyn Future<Output = Result<(), LaunchManagerError>> + Send + 'a>> {
        Box::pin(async move {
            let proxy = SystemdManagerProxy::new(&self.connection)
                .await
                .map_err(manager_error)?;
            let units = vec![USER_UNIT_NAME.to_owned()];
            if enabled {
                let _changes = proxy
                    .enable_unit_files(units, false, false)
                    .await
                    .map_err(manager_error)?;
            } else {
                let _changes = proxy
                    .disable_unit_files(units, false)
                    .await
                    .map_err(manager_error)?;
            }
            proxy.reload().await.map_err(manager_error)
        })
    }
}

#[allow(clippy::needless_pass_by_value)]
fn manager_error(error: zbus::Error) -> LaunchManagerError {
    LaunchManagerError {
        code: "launch-manager-unavailable",
        message: format!("the per-user systemd manager rejected the operation: {error}"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn adapter_never_spawns_systemctl_or_a_process() {
        let source = include_str!("systemd.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .map_or(source, |(production, _)| production);
        assert!(!production.contains("std::process::Command"));
        assert!(!production.contains("Command::new"));
        assert!(!production.contains(&["system", "ctl"].concat()));
    }
}
