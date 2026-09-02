use std::{fs, path::Path, time::Duration};

use crate::{model::OsdHardwareState, state::StateStore};

const LED_ROOT: &str = "/sys/class/leds";

pub(crate) async fn monitor(state: StateStore) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let value = tokio::task::spawn_blocking(|| read_state(Path::new(LED_ROOT)))
            .await
            .unwrap_or_else(|error| OsdHardwareState {
                error: Some(format!("LED reader failed: {error}")),
                ..OsdHardwareState::default()
            });
        state.update_osd_hardware(value).await;
    }
}

fn read_state(root: &Path) -> OsdHardwareState {
    let Ok(entries) = fs::read_dir(root) else {
        return OsdHardwareState {
            error: Some("LED class is unavailable".into()),
            ..OsdHardwareState::default()
        };
    };
    let mut state = OsdHardwareState {
        available: true,
        ..OsdHardwareState::default()
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let brightness = read_number(&entry.path().join("brightness")).unwrap_or(0);
        let maximum = read_number(&entry.path().join("max_brightness"))
            .unwrap_or(1)
            .max(1);
        if name.contains("capslock") {
            state.caps_lock |= brightness > 0;
        } else if name.contains("numlock") {
            state.num_lock |= brightness > 0;
        } else if name.contains("kbd_backlight") || name.contains("keyboard-backlight") {
            let percent = ((brightness.saturating_mul(100) / maximum).min(100)) as u8;
            state.keyboard_backlight_percent = Some(
                state
                    .keyboard_backlight_percent
                    .map_or(percent, |current| current.max(percent)),
            );
        } else if name.contains("micmute") || name.contains("microphone-mute") {
            state.microphone_privacy |= brightness > 0;
        } else if name.contains("cameramute") || name.contains("camera-mute") {
            state.camera_privacy |= brightness > 0;
        }
    }
    state
}

fn read_number(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::read_state;

    fn led(root: &std::path::Path, name: &str, brightness: u64, maximum: u64) {
        let path = root.join(name);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("brightness"), brightness.to_string()).unwrap();
        fs::write(path.join("max_brightness"), maximum.to_string()).unwrap();
    }

    #[test]
    fn reads_lock_backlight_and_privacy_leds() {
        let root = tempdir().unwrap();
        led(root.path(), "input3::capslock", 1, 1);
        led(root.path(), "input3::numlock", 0, 1);
        led(root.path(), "platform::kbd_backlight", 2, 4);
        led(root.path(), "platform::cameramute", 1, 1);
        let state = read_state(root.path());
        assert!(state.caps_lock);
        assert!(!state.num_lock);
        assert_eq!(state.keyboard_backlight_percent, Some(50));
        assert!(state.camera_privacy);
    }
}
