//! Claim the legacy writer's exclusive bus owner before touching agenda data.
use super::*;
const BUS_NAME: &str = "org.laufan.BarDaemon";
use tokio::io::{AsyncBufReadExt, BufReader};
#[tokio::test]
async fn existing_legacy_owner_blocks_startup_without_queueing_or_replacement() {
    let mut bus = tokio::process::Command::new("dbus-daemon")
        .args([
            "--config-file",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/authorization-session.conf"
            ),
            "--nofork",
            "--print-address=1",
        ])
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let address = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        BufReader::new(bus.stdout.take().unwrap())
            .lines()
            .next_line(),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let first = connection::Builder::address(address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    let second = connection::Builder::address(address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    first.request_name(BUS_NAME).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("todos.json");
    std::fs::write(&legacy, b"legacy snapshot must remain untouched").unwrap();
    let admitted = claim_names(&second, crate::component::Component::Activity, false).await;
    // This is the same startup admission barrier run() executes before Store::open.
    if admitted.is_ok() {
        let _ = ActivityService::with_paths(
            StateStore::default(),
            dir.path().join("config.json"),
            legacy.clone(),
        )
        .await;
    }
    assert!(admitted.is_err());
    assert_eq!(
        std::fs::read(&legacy).unwrap(),
        b"legacy snapshot must remain untouched"
    );
    assert!(!dir.path().join("agenda").exists());
    let proxy = zbus::fdo::DBusProxy::new(&second).await.unwrap();
    let owner = proxy
        .get_name_owner(BUS_NAME.try_into().unwrap())
        .await
        .unwrap();
    assert_eq!(owner.as_str(), first.unique_name().unwrap().as_str());
    first.release_name(BUS_NAME).await.unwrap();
    // An unresponsive status owner has no role in agenda exclusion.
    first
        .request_name(crate::component::Component::Bar.bus_name())
        .await
        .unwrap();
    claim_names(&second, crate::component::Component::Activity, false)
        .await
        .unwrap();
    // A legacy writer started later is excluded for the full activity lifetime.
    let reply = first
        .request_name_with_flags(BUS_NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        .await;
    assert!(matches!(reply, Err(zbus::Error::NameTaken)));
    bus.kill().await.unwrap();
}
