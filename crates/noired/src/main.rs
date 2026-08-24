//! Noire daemon process entry point.

#![forbid(unsafe_code)]

use clap::Parser;

/// Owns Noire's audio graph and control-plane state.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Arguments {
    /// Verify that this binary contains the production `PipeWire` backend.
    #[arg(long, hide = true)]
    verify_native_backend: bool,
}

#[cfg(not(feature = "runtime"))]
fn main() {
    let arguments = Arguments::parse();
    if arguments.verify_native_backend {
        eprintln!("noired was built without the runtime feature");
        std::process::exit(1);
    }
    println!("noired was built without the runtime feature");
}

#[cfg(feature = "runtime")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    use std::sync::Arc;

    use noire_config::ConfigStore;
    use noired::{AudioEngine, Daemon, NoireService, SystemdUserManager, register_and_claim};

    let arguments = Arguments::parse();
    if arguments.verify_native_backend {
        #[cfg(feature = "pipewire-backend")]
        {
            println!("pipewire-backend");
            return Ok(());
        }
        #[cfg(not(feature = "pipewire-backend"))]
        anyhow::bail!("noired was built without the production PipeWire backend");
    }
    tracing_subscriber::fmt().with_target(false).init();

    let connection = zbus::Connection::session().await?;
    let store = ConfigStore::discover()?;
    let loaded = store.load()?;
    #[cfg(feature = "pipewire-backend")]
    let engine: Box<dyn AudioEngine> = Box::new(noired::NativeAudioEngine::spawn()?);
    #[cfg(not(feature = "pipewire-backend"))]
    let engine: Box<dyn AudioEngine> = Box::new(noired::NullAudioEngine);
    let daemon = Daemon::new(store, engine, loaded);
    let launch_manager = Arc::new(SystemdUserManager::new(connection.clone()));
    // Expose the well-known name only after the object is ready. Otherwise
    // simultaneous D-Bus activation clients can deliver calls into the gap and
    // wait forever for replies that the object server never saw.
    let service = NoireService::new(daemon, launch_manager);
    register_and_claim(&connection, service.clone()).await?;
    tracing::info!(event = "daemon.ready", bus_name = noire_ipc::BUS_NAME);
    if let Some(controller_pid) = portable_controller_pid() {
        tokio::select! {
            () = service.monitor(&connection) => {
                tracing::info!(event = "daemon.session-closed");
            }
            () = wait_for_controller_exit(controller_pid) => {
                tracing::info!(event = "daemon.portable-controller-exited");
            }
        }
    } else {
        service.monitor(&connection).await;
        tracing::info!(event = "daemon.session-closed");
    }
    Ok(())
}

#[cfg(feature = "runtime")]
fn portable_controller_pid() -> Option<u32> {
    let pid = std::env::var("NOIRE_PORTABLE_CONTROLLER_PID")
        .ok()?
        .parse::<u32>()
        .ok()?;
    (pid > 1 && pid != std::process::id()).then_some(pid)
}

#[cfg(feature = "runtime")]
async fn wait_for_controller_exit(pid: u32) {
    let process = std::path::PathBuf::from(format!("/proc/{pid}"));
    while process.exists() {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}
