//! Startup failures must never be mistaken for native readiness. These tests
//! replace hypridle with a failing process; they never contact a real system bus.
use std::{fs, os::unix::net::UnixDatagram, path::Path, process::Command, time::Duration};

fn failed_start(executable: &Path) {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("base.conf");
    fs::write(&config, "listener {\n timeout=300\n on-timeout=true\n}\n").unwrap();
    let notify = root.path().join("notify");
    let socket = UnixDatagram::bind(&notify).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_bar-daemon"))
        .args(["idle", "--config"])
        .arg(config)
        .arg("--hypridle")
        .arg(executable)
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_RUNTIME_DIR", root.path())
        .env(
            "DBUS_SYSTEM_BUS_ADDRESS",
            format!("unix:path={}/no-system-bus", root.path().display()),
        )
        .env("NOTIFY_SOCKET", notify)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(
        root.path().join("bar-daemon/hypridle.conf").exists(),
        "generation was written before exec"
    );
    assert!(
        socket.recv(&mut [0; 512]).is_err(),
        "a generated file is not readiness"
    );
}

#[test]
fn missing_executable_does_not_announce_readiness() {
    // The harness exists in the Nix sandbox and rejects hypridle's --config.
    for executable in [
        Path::new("/nonexistent-shelllist-hypridle").to_path_buf(),
        std::env::current_exe().unwrap(),
    ] {
        failed_start(&executable);
    }
}
