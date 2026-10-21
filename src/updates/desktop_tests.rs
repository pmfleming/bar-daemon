use ksni::TrayMethods;
use std::sync::{Arc, Mutex};

struct Watcher(Arc<Mutex<Vec<String>>>);
#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_item(&self, service: String) {
        self.0.lock().unwrap().push(service);
    }
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

#[test]
fn real_sni_registration_activation_and_watcher_restart() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let launcher = root.path().join("shelllist");
    std::fs::write(
        &launcher,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$UPDATES_TRAY_RESULT\"\n",
    )
    .unwrap();
    // Use the actual shell path: NixOS need not provide /bin/sh to pure builds.
    let shell = std::process::Command::new("sh")
        .arg("-c")
        .arg("command -v sh")
        .output()
        .unwrap();
    let text = std::fs::read_to_string(&launcher).unwrap().replacen(
        "/bin/sh",
        String::from_utf8_lossy(&shell.stdout).trim(),
        1,
    );
    std::fs::write(&launcher, text).unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    let result = std::process::Command::new("dbus-run-session")
        // Pure builds have no host /etc/dbus-1/session.conf. Keep this bus
        // isolated from host configuration and service activation.
        .arg(concat!(
            "--config-file=",
            env!("CARGO_MANIFEST_DIR"),
            "/test_support/dbus-session.conf"
        ))
        .arg("--")
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "updates::desktop_tests::private_bus_child",
            "--nocapture",
        ])
        .env("UPDATES_TRAY_CHILD", "1")
        .env("BAR_DAEMON_SHELLLIST", &launcher)
        .env("UPDATES_TRAY_RESULT", root.path().join("result"))
        .env(
            "PATH",
            format!(
                "{}:{}",
                root.path().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("result")).unwrap(),
        "updates\nopen\n"
    );
}

#[tokio::test]
async fn private_bus_child() {
    if std::env::var("UPDATES_TRAY_CHILD").as_deref() != Ok("1") {
        return;
    }
    let calls = Arc::new(Mutex::new(Vec::new()));
    // Applications may start before the shell's watcher. Registration must not
    // depend on a watcher being present when the Updates item is constructed.
    let handle = super::tray::Tray
        .assume_sni_available(true)
        .spawn()
        .await
        .unwrap();
    let watcher = zbus::connection::Builder::session()
        .unwrap()
        .name("org.kde.StatusNotifierWatcher")
        .unwrap()
        .serve_at("/StatusNotifierWatcher", Watcher(calls.clone()))
        .unwrap()
        .build()
        .await
        .unwrap();
    let wait = async {
        while calls.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), wait)
        .await
        .unwrap();
    let service = calls.lock().unwrap()[0].clone();
    let proxy = zbus::Proxy::new(
        &watcher,
        service.as_str(),
        "/StatusNotifierItem",
        "org.kde.StatusNotifierItem",
    )
    .await
    .unwrap();
    assert_eq!(
        proxy.get_property::<String>("Id").await.unwrap(),
        "shelllist-updates"
    );
    assert_eq!(
        proxy.get_property::<String>("Status").await.unwrap(),
        "Active"
    );
    let menu: zvariant::OwnedObjectPath = proxy.get_property("Menu").await.unwrap();
    let menu_proxy = zbus::Proxy::new(
        &watcher,
        service.as_str(),
        menu.as_str(),
        "com.canonical.dbusmenu",
    )
    .await
    .unwrap();
    let label: zvariant::OwnedValue = menu_proxy
        .call("GetProperty", &(1i32, "label"))
        .await
        .unwrap();
    assert_eq!(String::try_from(label).unwrap(), "Open update history");
    let _: () = proxy.call("Activate", &(0i32, 0i32)).await.unwrap();
    let result = std::path::PathBuf::from(std::env::var_os("UPDATES_TRAY_RESULT").unwrap());
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !result.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(proxy);
    drop(menu_proxy);
    drop(watcher);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let _watcher = zbus::connection::Builder::session()
        .unwrap()
        .name("org.kde.StatusNotifierWatcher")
        .unwrap()
        .serve_at("/StatusNotifierWatcher", Watcher(calls.clone()))
        .unwrap()
        .build()
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while calls.lock().unwrap().len() < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    handle.shutdown().await;
}
