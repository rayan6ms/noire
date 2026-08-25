//! Scriptable D-Bus control client for the Noire daemon.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use noire_ipc::{Noire1Proxy, Snapshot};
use serde::Serialize;

/// Inspects and controls the Noire daemon.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Arguments {
    /// Emit stable schema-versioned JSON.
    #[arg(long, global = true)]
    json: bool,
    /// Control or inspection operation.
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Return the complete daemon snapshot.
    Status,
    /// List stable input descriptors.
    Devices,
    /// Persist and start processing.
    Start(RevisionArgument),
    /// Persist and stop processing.
    Stop(RevisionArgument),
    /// Change one validated setting.
    Set {
        /// Expected revision; omitted means fetch immediately before mutation.
        #[arg(long)]
        revision: Option<u64>,
        /// Setting mutation.
        #[command(subcommand)]
        setting: Setting,
    },
    /// Request immediate recovery without changing intended configuration.
    Retry(RevisionArgument),
    /// Return a sanitized report containing no audio or raw environment dump.
    Diagnostics,
}

#[derive(Clone, Debug, clap::Args)]
struct RevisionArgument {
    /// Expected revision; omitted means fetch immediately before mutation.
    #[arg(long)]
    revision: Option<u64>,
}

#[derive(Debug, Subcommand)]
enum Setting {
    /// Select a persistable `PipeWire` input ID.
    Input {
        /// Stable ID from `noirectl devices`, or `default` to follow the default.
        stable_id: String,
    },
    /// Enable or bypass suppression with a smooth transition.
    Enabled {
        /// True enables processed output; false selects delayed dry output.
        #[arg(action = clap::ArgAction::Set)]
        enabled: bool,
    },
    /// Set wet strength from 0.0 through 1.0.
    Strength {
        /// Inclusive suppression strength.
        strength: f64,
    },
    /// Select low or balanced buffering.
    LatencyProfile {
        /// Stable profile name.
        profile: String,
    },
    /// Select closed or explicitly opted-in open failure behavior.
    FailMode {
        /// Stable mode name.
        mode: String,
    },
    /// Enable or disable the user unit at login through systemd D-Bus.
    LaunchAtLogin {
        /// Desired enablement.
        #[arg(action = clap::ArgAction::Set)]
        enabled: bool,
    },
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Arguments, Command, Setting, error_code};

    #[test]
    fn explicit_false_boolean_settings_are_parseable() -> Result<(), clap::Error> {
        let enabled = Arguments::try_parse_from(["noirectl", "set", "enabled", "false"])?;
        assert!(matches!(
            enabled.command,
            Command::Set {
                setting: Setting::Enabled { enabled: false },
                ..
            }
        ));
        let login = Arguments::try_parse_from(["noirectl", "set", "launch-at-login", "false"])?;
        assert!(matches!(
            login.command,
            Command::Set {
                setting: Setting::LaunchAtLogin { enabled: false },
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn rollback_failures_keep_their_stable_error_code() {
        assert_eq!(
            error_code("io.github.rayan6ms.Noire.Noire1.Error.RollbackFailed"),
            "config-rollback-failed"
        );
    }
}

#[derive(Serialize)]
struct ErrorEnvelope {
    schema_version: u32,
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: String,
    message: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let arguments = Arguments::parse();
    match run(&arguments).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let message = error.to_string();
            if arguments.json {
                let envelope = ErrorEnvelope {
                    schema_version: 1,
                    error: ErrorBody {
                        code: error_code(&message).to_owned(),
                        message,
                    },
                };
                if let Ok(json) = serde_json::to_string(&envelope) {
                    eprintln!("{json}");
                }
            } else {
                eprintln!("noirectl: {message}");
            }
            ExitCode::from(2)
        }
    }
}

async fn run(arguments: &Arguments) -> anyhow::Result<()> {
    let connection = zbus::Connection::session().await?;
    let proxy = Noire1Proxy::new(&connection).await?;
    match &arguments.command {
        Command::Status => print_value(arguments.json, &proxy.get_snapshot().await?)?,
        Command::Devices => print_value(arguments.json, &proxy.list_inputs().await?)?,
        Command::Start(revision) => {
            let revision = expected_revision(&proxy, revision.revision).await?;
            print_value(arguments.json, &proxy.start(revision).await?)?;
        }
        Command::Stop(revision) => {
            let revision = expected_revision(&proxy, revision.revision).await?;
            print_value(arguments.json, &proxy.stop(revision).await?)?;
        }
        Command::Retry(revision) => {
            let revision = expected_revision(&proxy, revision.revision).await?;
            print_value(arguments.json, &proxy.retry(revision).await?)?;
        }
        Command::Diagnostics => print_value(arguments.json, &proxy.diagnostics().await?)?,
        Command::Set { revision, setting } => {
            let revision = expected_revision(&proxy, *revision).await?;
            let snapshot = match setting {
                Setting::Input { stable_id } => {
                    let stable_id = if stable_id == "default" {
                        ""
                    } else {
                        stable_id.as_str()
                    };
                    proxy.select_input(stable_id, revision).await?
                }
                Setting::Enabled { enabled } => {
                    proxy.set_suppression_enabled(*enabled, revision).await?
                }
                Setting::Strength { strength } => proxy.set_strength(*strength, revision).await?,
                Setting::LatencyProfile { profile } => {
                    proxy.set_latency_profile(profile, revision).await?
                }
                Setting::FailMode { mode } => proxy.set_fail_mode(mode, revision).await?,
                Setting::LaunchAtLogin { enabled } => {
                    proxy.set_launch_at_login(*enabled, revision).await?
                }
            };
            print_value(arguments.json, &snapshot)?;
        }
    }
    Ok(())
}

async fn expected_revision(proxy: &Noire1Proxy<'_>, provided: Option<u64>) -> zbus::Result<u64> {
    match provided {
        Some(revision) => Ok(revision),
        None => proxy.get_snapshot().await.map(|snapshot| snapshot.revision),
    }
}

fn print_value<T: Serialize>(json: bool, value: &T) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string(value)?);
    } else {
        let value = serde_json::to_value(value)?;
        if let Ok(snapshot) = serde_json::from_value::<Snapshot>(value.clone()) {
            println!(
                "state={} active={} revision={} input={} suppression={} strength={:.3} error={}",
                snapshot.state,
                snapshot.active,
                snapshot.revision,
                if snapshot.input_display_name.is_empty() {
                    snapshot.input_mode
                } else {
                    snapshot.input_display_name
                },
                snapshot.suppression_enabled,
                snapshot.strength,
                if snapshot.has_error {
                    snapshot.last_error.code
                } else {
                    "none".to_owned()
                }
            );
        } else {
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
    }
    Ok(())
}

fn error_code(message: &str) -> &'static str {
    if message.contains(".Conflict") {
        "conflict"
    } else if message.contains(".InvalidArgument") {
        "invalid-argument"
    } else if message.contains(".RollbackFailed") {
        "config-rollback-failed"
    } else if message.contains(".Unavailable") {
        "unavailable"
    } else if message.contains(".Persistence") {
        "persistence"
    } else if message.contains(".LaunchManager") {
        "launch-manager"
    } else if message.contains(".Busy") {
        "busy"
    } else {
        "service-unavailable"
    }
}
