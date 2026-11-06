use std::{
    fs::{self, File},
    os::unix::fs::FileExt,
    path::Path,
    time::Duration,
};

use tokio::{
    io::{Interest, unix::AsyncFd},
    sync::mpsc,
    task::JoinSet,
};

use crate::{model::OsdHardwareState, state::StateStore};

const LED_ROOT: &str = "/sys/class/leds";
// Most LED drivers do not notify on `brightness` changes. Cache the relevant
// open files/maxima so the compatibility path stays cheap and responsive.
const FALLBACK_INTERVAL: Duration = Duration::from_millis(50);
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) async fn monitor(state: StateStore) {
    let (changes, mut events) = mpsc::channel(1);
    let mut watchers = JoinSet::new();
    let mut reader = LedReader::default();
    let mut poll = tokio::time::interval(FALLBACK_INTERVAL);
    let mut discover = tokio::time::interval(DISCOVERY_INTERVAL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    discover.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let rescan = tokio::select! {
            biased;
            _ = discover.tick() => true,
            _ = events.recv() => false,
            _ = poll.tick() => false,
        };
        // Sysfs reads may enter firmware; never perform them on a Tokio worker.
        let result = tokio::task::spawn_blocking(move || reader.sample(rescan)).await;
        let (next, notifications, value) = result.unwrap_or_else(|error| {
            (
                LedReader::default(),
                None,
                OsdHardwareState {
                    error: Some(format!("LED reader failed: {error}")),
                    ..OsdHardwareState::default()
                },
            )
        });
        reader = next;
        if let Some(files) = notifications {
            // Dropping the old set cancels readiness waits and closes old fds.
            watchers = hardware_watchers(files, &changes);
        }
        // Reap failed watches; periodic discovery retries them after hotplug.
        while watchers.try_join_next().is_some() {}
        state.update_osd_hardware(value).await;
    }
}

fn hardware_watchers(files: Vec<File>, changes: &mpsc::Sender<()>) -> JoinSet<()> {
    files
        .into_iter()
        .filter_map(|file| AsyncFd::with_interest(file, Interest::PRIORITY).ok())
        .map(|file| watch_hardware_changes(file, changes.clone()))
        .collect()
}

async fn watch_hardware_changes(file: AsyncFd<File>, changes: mpsc::Sender<()>) {
    loop {
        let Ok(mut ready) = file.ready(Interest::PRIORITY).await else {
            return;
        };
        // sysfs_notify is acknowledged by rereading from offset zero, not by
        // inotify or ordinary readable readiness (sysfs is always readable).
        if file.get_ref().read_at(&mut [0; 32], 0).is_err() {
            return;
        }
        ready.clear_ready();
        let _ = changes.try_send(());
    }
}

#[derive(Default)]
struct LedReader {
    available: bool,
    leds: Vec<Led>,
}

struct Led {
    kind: LedKind,
    brightness: File,
    maximum: u64,
}

impl LedReader {
    fn sample(mut self, rescan: bool) -> (Self, Option<Vec<File>>, OsdHardwareState) {
        let notifications = rescan.then(|| {
            let (next, files) = Self::discover(Path::new(LED_ROOT));
            self = next;
            files
        });
        let value = self.read_state();
        (self, notifications, value)
    }

    fn discover(root: &Path) -> (Self, Vec<File>) {
        let Ok(entries) = fs::read_dir(root) else {
            return (Self::default(), Vec::new());
        };
        let mut reader = Self {
            available: true,
            leds: Vec::new(),
        };
        let mut notifications = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let Some(kind) = led_kind(&name) else {
                continue;
            };
            let path = entry.path();
            let Ok(brightness) = File::open(path.join("brightness")) else {
                continue;
            };
            let maximum = File::open(path.join("max_brightness"))
                .ok()
                .as_ref()
                .and_then(read_number)
                .unwrap_or(1)
                .max(1);
            reader.leds.push(Led {
                kind,
                brightness,
                maximum,
            });
            // Optional Linux LED-class hardware notification. Keep the 50 ms
            // fallback too: software writes need not emit hardware events.
            if let Ok(file) = File::open(path.join("brightness_hw_changed"))
                && file.read_at(&mut [0; 32], 0).is_ok()
            {
                notifications.push(file);
            }
        }
        (reader, notifications)
    }

    fn read_state(&self) -> OsdHardwareState {
        if !self.available {
            return OsdHardwareState {
                error: Some("LED class is unavailable".into()),
                ..OsdHardwareState::default()
            };
        }
        let mut state = OsdHardwareState {
            available: true,
            ..OsdHardwareState::default()
        };
        for led in &self.leds {
            if let Some(brightness) = read_number(&led.brightness) {
                update_led(&mut state, led.kind, brightness, led.maximum);
            }
        }
        state
    }
}

#[derive(Clone, Copy)]
enum LedKind {
    CapsLock,
    NumLock,
    Keyboard,
    Microphone,
    Camera,
}

fn update_led(state: &mut OsdHardwareState, kind: LedKind, brightness: u64, maximum: u64) {
    let active = brightness > 0;
    match kind {
        LedKind::CapsLock => state.caps_lock |= active,
        LedKind::NumLock => state.num_lock |= active,
        LedKind::Keyboard => {
            let percent = ((brightness.saturating_mul(100) / maximum).min(100)) as u8;
            state.keyboard_backlight_percent = Some(
                state
                    .keyboard_backlight_percent
                    .map_or(percent, |current| current.max(percent)),
            );
        }
        LedKind::Microphone => state.microphone_privacy |= active,
        LedKind::Camera => state.camera_privacy |= active,
    }
}

fn led_kind(name: &str) -> Option<LedKind> {
    [
        ("capslock", LedKind::CapsLock),
        ("numlock", LedKind::NumLock),
        ("kbd_backlight", LedKind::Keyboard),
        ("keyboard-backlight", LedKind::Keyboard),
        ("micmute", LedKind::Microphone),
        ("microphone-mute", LedKind::Microphone),
        ("cameramute", LedKind::Camera),
        ("camera-mute", LedKind::Camera),
    ]
    .into_iter()
    .find_map(|(pattern, kind)| name.contains(pattern).then_some(kind))
}

fn read_number(file: &File) -> Option<u64> {
    let mut bytes = [0; 32];
    let size = file.read_at(&mut bytes, 0).ok()?;
    std::str::from_utf8(&bytes[..size])
        .ok()?
        .trim()
        .parse()
        .ok()
}
