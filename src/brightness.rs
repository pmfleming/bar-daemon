use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use notify::{Config, PollWatcher, RecursiveMode, Watcher};
use tokio::{process::Command, sync::mpsc, time::sleep};

use crate::{model::BrightnessState, state::StateStore};

const DEFAULT_BACKLIGHT_ROOT: &str = "/sys/class/backlight";

#[derive(Debug, Clone)]
struct BacklightDevice {
    name: String,
    path: PathBuf,
    brightness: u64,
    max_brightness: u64,
}

impl BacklightDevice {
    fn state(&self) -> BrightnessState {
        BrightnessState {
            available: true,
            device: self.name.clone(),
            brightness: self.brightness,
            max_brightness: self.max_brightness,
            percent: percent(self.brightness, self.max_brightness),
            error: None,
        }
    }
}

pub async fn monitor(store: StateStore) {
    let root = PathBuf::from(
        std::env::var_os("BAR_DAEMON_BACKLIGHT_ROOT")
            .unwrap_or_else(|| DEFAULT_BACKLIGHT_ROOT.into()),
    );
    refresh(&store, &root).await;
    let (tx, mut rx) = mpsc::channel(8);
    let mut watcher = PollWatcher::new(
        move |result: notify::Result<notify::Event>| {
            if result.is_ok() {
                let _ = tx.try_send(());
            }
        },
        Config::default()
            .with_poll_interval(Duration::from_secs(2))
            .with_compare_contents(true),
    )
    .ok();
    if let (Some(watcher), Ok(device)) = (watcher.as_mut(), discover(&root)) {
        for file in ["actual_brightness", "brightness", "max_brightness"] {
            let path = device.path.join(file);
            if path.exists() {
                if let Err(error) = watcher.watch(&path, RecursiveMode::NonRecursive) {
                    tracing::debug!(%error, path = %path.display(), "backlight file watcher unavailable");
                }
            }
        }
    }
    loop {
        refresh(&store, &root).await;
        tokio::select! {
            value = rx.recv() => if value.is_none() { sleep(Duration::from_secs(2)).await; },
            _ = sleep(Duration::from_secs(30)) => {}
        }
    }
}

async fn refresh(store: &StateStore, root: &Path) {
    let root = root.to_path_buf();
    match tokio::task::spawn_blocking(move || discover(&root)).await {
        Ok(Ok(device)) => store.update_brightness(device.state()).await,
        Ok(Err(error)) => {
            store
                .update_brightness(BrightnessState {
                    error: Some(error.to_string()),
                    ..BrightnessState::default()
                })
                .await
        }
        Err(error) => {
            store
                .update_brightness(BrightnessState {
                    error: Some(error.to_string()),
                    ..BrightnessState::default()
                })
                .await
        }
    }
}

pub async fn adjust(delta_percent: i16) -> Result<BrightnessState> {
    let device = discover(Path::new(DEFAULT_BACKLIGHT_ROOT))?;
    let current = i64::from(percent(device.brightness, device.max_brightness));
    set_for_device(
        device,
        (current + i64::from(delta_percent)).clamp(1, 100) as u8,
    )
    .await
}

pub async fn set(percent: u8) -> Result<BrightnessState> {
    if !(1..=100).contains(&percent) {
        bail!("brightness percent must be between 1 and 100");
    }
    let device = discover(Path::new(DEFAULT_BACKLIGHT_ROOT))?;
    set_for_device(device, percent).await
}

async fn set_for_device(device: BacklightDevice, requested_percent: u8) -> Result<BrightnessState> {
    let target = raw_brightness(device.max_brightness, requested_percent);
    let direct_result = tokio::fs::write(device.path.join("brightness"), target.to_string()).await;
    if direct_result.is_err() {
        let output = Command::new("brightnessctl")
            .args([
                "--device",
                &device.name,
                "set",
                &target.to_string(),
                "--quiet",
            ])
            .output()
            .await
            .context("start brightnessctl")?;
        if !output.status.success() {
            bail!(
                "brightnessctl failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
    }
    let root = device
        .path
        .parent()
        .context("backlight device has no parent")?;
    Ok(discover(root)?.state())
}

fn discover(root: &Path) -> Result<BacklightDevice> {
    let mut devices = Vec::new();
    for entry in std::fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let max_brightness = read_u64(&path.join("max_brightness"))?;
        // Use the requested value for control state. `actual_brightness` may lag
        // behind it or report a slightly different hardware level; basing each
        // adjustment on that value makes repeated percentage steps drift.
        let brightness = read_u64(&path.join("brightness"))
            .or_else(|_| read_u64(&path.join("actual_brightness")))?;
        if max_brightness > 0 {
            devices.push(BacklightDevice {
                name,
                path,
                brightness,
                max_brightness,
            });
        }
    }
    devices
        .into_iter()
        .max_by_key(|device| device.max_brightness)
        .context("no backlight device is available")
}

fn read_u64(path: &Path) -> Result<u64> {
    std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?
        .trim()
        .parse()
        .with_context(|| format!("parse {}", path.display()))
}

fn percent(brightness: u64, maximum: u64) -> u8 {
    if maximum == 0 {
        return 0;
    }
    ((u128::from(brightness) * 100 + u128::from(maximum) / 2) / u128::from(maximum)).min(100) as u8
}

fn raw_brightness(maximum: u64, requested_percent: u8) -> u64 {
    if maximum == 0 {
        return 0;
    }
    let rounded = (u128::from(maximum) * u128::from(requested_percent) + 50) / 100;
    (rounded as u64).clamp(1, maximum)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use tempfile::tempdir;

    use super::{discover, percent, raw_brightness};

    #[test]
    fn discovers_highest_resolution_backlight() {
        let root = tempdir().unwrap();
        for (name, current, maximum) in [("small", 5, 10), ("panel", 600, 1000)] {
            let path = root.path().join(name);
            fs::create_dir(&path).unwrap();
            fs::write(path.join("actual_brightness"), current.to_string()).unwrap();
            fs::write(path.join("max_brightness"), maximum.to_string()).unwrap();
        }
        let device = discover(root.path()).unwrap();
        assert_eq!(device.name, "panel");
        assert_eq!(device.state().percent, 60);
    }

    #[test]
    fn prefers_requested_brightness_over_hardware_feedback() {
        let root = tempdir().unwrap();
        let path = root.path().join("panel");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("brightness"), "600").unwrap();
        fs::write(path.join("actual_brightness"), "550").unwrap();
        fs::write(path.join("max_brightness"), "1000").unwrap();

        let state = discover(root.path()).unwrap().state();
        assert_eq!(state.brightness, 600);
        assert_eq!(state.percent, 60);
    }

    #[test]
    fn rounds_and_bounds_percentages() {
        assert_eq!(percent(1, 3), 33);
        assert_eq!(percent(200, 100), 100);
        assert_eq!(percent(1, 0), 0);
    }

    #[test]
    fn nonzero_percent_never_turns_off_low_resolution_backlight() {
        assert_eq!(raw_brightness(10, 1), 1);
        assert_eq!(raw_brightness(10, 5), 1);
        assert_eq!(raw_brightness(10, 100), 10);
    }
}
