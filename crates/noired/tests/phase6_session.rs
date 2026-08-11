//! Same-user D-Bus acceptance scenarios. Run inside `dbus-run-session`.

#![cfg(feature = "runtime")]

use std::{
    error::Error,
    fs,
    future::{self, Future},
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use noire_config::{Config, ConfigStore, LoadOutcome, LoadSource};
use noire_ipc::{InputDescriptor, Noire1Proxy};
use noired::{
    AudioEngine, Daemon, EngineError, EngineObservation, LaunchManager, LaunchManagerError,
    LifecycleState, NoireService, claim_name, register_service,
};

#[derive(Default)]
struct FakeEngine;

impl AudioEngine for FakeEngine {
    fn apply(&mut self, config: &Config) -> Result<EngineObservation, EngineError> {
        Ok(EngineObservation {
            state: if config.active {
                LifecycleState::Running
            } else {
                LifecycleState::Stopped
            },
            input_display_name: "Session Test Microphone".to_owned(),
            pipewire_version: "test-1.0".to_owned(),
            ..EngineObservation::default()
        })
    }

    fn inputs(&mut self) -> Result<Vec<InputDescriptor>, EngineError> {
        Ok(vec![InputDescriptor {
            stable_id: "alsa_input.session-test".to_owned(),
            display_name: "Session Test Microphone".to_owned(),
            is_default: true,
            availability: "available".to_owned(),
        }])
    }
}

#[derive(Default)]
struct FakeLaunchManager;

impl LaunchManager for FakeLaunchManager {
    fn set_enabled<'a>(
        &'a self,
        _enabled: bool,
    ) -> Pin<Box<dyn Future<Output = Result<(), LaunchManagerError>> + Send + 'a>> {
        Box::pin(future::ready(Ok(())))
    }
}

fn temporary_store() -> Result<(PathBuf, ConfigStore), Box<dyn Error>> {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "noire-phase6-session-{}-{nonce}",
        std::process::id()
    ));
    Ok((
        root.clone(),
        ConfigStore::new(root.join("noire/config.toml")),
    ))
}

#[tokio::test]
#[ignore = "requires a private dbus-run-session"]
#[allow(clippy::too_many_lines)]
async fn same_user_contract_rejects_stale_invalid_and_malformed_requests()
-> Result<(), Box<dyn Error>> {
    let (root, store) = temporary_store()?;
    let server = zbus::Connection::session().await?;
    claim_name(&server).await?;
    let daemon = Daemon::new(
        store,
        Box::new(FakeEngine),
        LoadOutcome {
            config: Config::default(),
            source: LoadSource::Defaults,
            warning: None,
            writable: true,
        },
    );
    register_service(
        &server,
        NoireService::new(daemon, Arc::new(FakeLaunchManager)),
    )
    .await?;
    let competing_daemon = zbus::Connection::session().await?;
    assert!(claim_name(&competing_daemon).await.is_err());

    let client = zbus::Connection::session().await?;
    let proxy = Noire1Proxy::new(&client).await?;
    let second_client = zbus::Connection::session().await?;
    let second_proxy = Noire1Proxy::new(&second_client).await?;
    let initial = proxy.get_snapshot().await?;
    assert_eq!(initial.revision, 1);
    assert_eq!(
        second_proxy.get_snapshot().await?.revision,
        initial.revision
    );
    assert_eq!(proxy.list_inputs().await?.len(), 1);
    let introspection = zbus::fdo::IntrospectableProxy::builder(&client)
        .destination(noire_ipc::BUS_NAME)?
        .path(noire_ipc::OBJECT_PATH)?
        .build()
        .await?
        .introspect()
        .await?;
    for member in [
        "GetSnapshot",
        "SetStrength",
        "StateChanged",
        "StateRevision",
        "DeviceRevision",
    ] {
        assert!(introspection.contains(&format!("name=\"{member}\"")));
    }

    let start = Instant::now();
    let started = proxy.start(initial.revision).await?;
    assert!(started.active);
    assert_eq!(started.state, "running");
    assert!(start.elapsed() < Duration::from_millis(500));

    let stale = second_proxy.set_strength(0.5, initial.revision).await;
    assert!(stale.is_err());
    assert!(format!("{}", stale.err().ok_or("missing stale error")?).contains("Conflict"));
    let invalid = proxy.set_strength(f64::NAN, started.revision).await;
    assert!(invalid.is_err());
    assert!(
        format!("{}", invalid.err().ok_or("missing invalid error")?).contains("InvalidArgument")
    );

    let malformed = client
        .call_method(
            Some(noire_ipc::BUS_NAME),
            noire_ipc::OBJECT_PATH,
            Some(noire_ipc::INTERFACE_NAME),
            "Start",
            &("not-a-revision",),
        )
        .await;
    assert!(malformed.is_err());

    let mut current = proxy.get_snapshot().await?;
    let mut timings = Vec::with_capacity(100);
    for index in 0..100 {
        let before = Instant::now();
        current = proxy
            .set_suppression_enabled(index % 2 == 0, current.revision)
            .await?;
        timings.push(before.elapsed());
        assert_eq!(proxy.get_snapshot().await?.revision, current.revision);
    }
    timings.sort_unstable();
    let p95 = timings[94];
    let maximum = timings[99];
    assert!(p95 < Duration::from_millis(500), "p95 was {p95:?}");
    println!(
        "NOIRE_PHASE6_CONTROL samples=100 p95_us={} max_us={}",
        p95.as_micros(),
        maximum.as_micros()
    );

    current = proxy.set_launch_at_login(true, current.revision).await?;
    assert!(current.launch_at_login);
    drop(proxy);
    drop(client);
    drop(second_proxy);
    drop(second_client);

    let replacement_client = zbus::Connection::session().await?;
    let replacement = Noire1Proxy::new(&replacement_client).await?;
    let after_client_close = replacement.get_snapshot().await?;
    assert!(after_client_close.active);
    assert_eq!(after_client_close.revision, current.revision);
    let report = replacement.diagnostics().await?;
    assert!(report.privacy.contains("no audio"));
    assert!(
        !report
            .journal_hint
            .contains(root.to_string_lossy().as_ref())
    );

    fs::remove_dir_all(root)?;
    Ok(())
}
