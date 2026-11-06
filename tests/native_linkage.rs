//! Verify actual ELF dependencies of every owner, including debug builds.
use std::process::Command;

#[test]
fn session_owners_link_only_their_native_providers() {
    for (binary, pipewire, udev) in [
        (env!("CARGO_BIN_EXE_bar-daemon"), true, false),
        (env!("CARGO_BIN_EXE_bar-power-daemon"), false, true),
        (env!("CARGO_BIN_EXE_bar-compositor-daemon"), false, false),
        (env!("CARGO_BIN_EXE_bar-activity-daemon"), false, false),
        (env!("CARGO_BIN_EXE_bar-notification-daemon"), true, false),
    ] {
        let result = Command::new("readelf")
            .args(["-d", binary])
            .output()
            .expect("readelf from the development/build toolchain");
        assert!(result.status.success());
        let entries = String::from_utf8(result.stdout).unwrap();
        let needed = entries
            .lines()
            .filter(|line| line.contains("NEEDED"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            needed.contains("libpipewire"),
            pipewire,
            "{binary}: {needed}"
        );
        assert_eq!(needed.contains("libudev"), udev, "{binary}: {needed}");
    }
}
