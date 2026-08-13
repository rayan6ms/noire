//! Noire daemon process entry point.

#![forbid(unsafe_code)]

use clap::Parser;

/// Owns Noire's audio graph and control-plane state.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Arguments;

#[cfg(not(feature = "runtime"))]
fn main() {
    Arguments::parse();
    println!("noired was built without the runtime feature");
}

#[cfg(feature = "runtime")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    use std::sync::Arc;

    use noire_config::ConfigStore;
    use noired::{
        AudioEngine, Daemon, NoireService, SystemdUserManager, claim_name, register_service,
    };

    Arguments::parse();
    tracing_subscriber::fmt().with_target(false).init();

    let connection = zbus::Connection::session().await?;
    claim_name(&connection).await?;
    let store = ConfigStore::discover()?;
    let loaded = store.load()?;
    #[cfg(feature = "pipewire-backend")]
    let engine: Box<dyn AudioEngine> = Box::new(noired::NativeAudioEngine::spawn()?);
    #[cfg(not(feature = "pipewire-backend"))]
    let engine: Box<dyn AudioEngine> = Box::new(noired::NullAudioEngine);
    let daemon = Daemon::new(store, engine, loaded);
    let launch_manager = Arc::new(SystemdUserManager::new(connection.clone()));
    register_service(&connection, NoireService::new(daemon, launch_manager)).await?;
    tracing::info!(event = "daemon.ready", bus_name = noire_ipc::BUS_NAME);
    connection.closed().await;
    tracing::info!(event = "daemon.session-closed");
    Ok(())
}
