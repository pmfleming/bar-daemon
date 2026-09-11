use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use notify::{Config, PollWatcher, RecursiveMode, Watcher};
use tokio::{
    process::Command,
    sync::{Mutex, mpsc},
    time::{sleep, timeout},
};

use crate::{model::BrightnessState, state::StateStore};

const DEFAULT_BACKLIGHT_ROOT: &str = "/sys/class/backlight";
const FALLBACK_TIMEOUT: Duration = Duration::from_secs(2);

/// One configuration and transaction boundary for observation and control.
#[derive(Clone)]
pub(crate) struct BrightnessService {
    state: StateStore,
    root: PathBuf,
    gate: Arc<Mutex<()>>,
    fallback: Option<PathBuf>,
}

impl BrightnessService {
    pub(crate) fn new(state: StateStore) -> Self {
        match std::env::var_os("BAR_DAEMON_BACKLIGHT_ROOT") {
            Some(root) => Self::with_root(state, root.into()),
            None => Self {
                fallback: Some("brightnessctl".into()),
                ..Self::with_root(state, DEFAULT_BACKLIGHT_ROOT.into())
            },
        }
    }

    // An explicit root is isolated: never let a failed fixture write reach the
    // real machine through brightnessctl (even if device names happen to match).
    fn with_root(state: StateStore, root: PathBuf) -> Self {
        Self {
            state,
            root,
            gate: Arc::new(Mutex::new(())),
            fallback: None,
        }
    }

    async fn refresh(&self) {
        let _guard = self.gate.lock().await;
        let root = self.root.clone();
        let result = tokio::task::spawn_blocking(move || discover(&root)).await;
        let state = match result {
            Ok(Ok(device)) => device.state(),
            Ok(Err(error)) => unavailable(error.to_string()),
            Err(error) => unavailable(error.to_string()),
        };
        self.state.update_brightness(state).await;
    }

    pub(crate) async fn adjust(&self, delta_percent: i16) -> Result<BrightnessState> {
        let _guard = self.gate.lock().await;
        let device = discover(&self.root)?;
        let current = i64::from(percent(device.brightness, device.max_brightness));
        self.apply(
            device,
            (current + i64::from(delta_percent)).clamp(1, 100) as u8,
        )
        .await
    }

    pub(crate) async fn set(&self, percent: u8) -> Result<BrightnessState> {
        if !(1..=100).contains(&percent) {
            bail!("brightness percent must be between 1 and 100");
        }
        let _guard = self.gate.lock().await;
        self.apply(discover(&self.root)?, percent).await
    }

    // Caller holds gate until the readback is committed and broadcast.
    async fn apply(&self, device: BacklightDevice, percent: u8) -> Result<BrightnessState> {
        let state = set_for_device(device, percent, self.fallback.as_deref()).await?;
        self.state.update_brightness(state.clone()).await;
        Ok(state)
    }

    pub(crate) async fn monitor(self) {
        self.refresh().await;
        let (tx, mut rx) = mpsc::channel(8);
        let mut watcher = backlight_watcher(tx);
        if let Some(watcher) = watcher.as_mut() {
            watch_device_files(watcher, &self.root);
        }
        loop {
            tokio::select! {
                value = rx.recv() => {
                    if value.is_some() {
                        sleep(Duration::from_millis(50)).await;
                        while rx.try_recv().is_ok() {}
                    } else {
                        sleep(Duration::from_secs(2)).await;
                    }
                },
                _ = sleep(Duration::from_secs(30)) => {}
            }
            self.refresh().await;
        }
    }
}

fn unavailable(error: String) -> BrightnessState {
    BrightnessState {
        error: Some(error),
        ..BrightnessState::default()
    }
}

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

fn backlight_watcher(tx: mpsc::Sender<()>) -> Option<PollWatcher> {
    PollWatcher::new(
        move |result: notify::Result<notify::Event>| {
            if result.is_ok() {
                let _ = tx.try_send(());
            }
        },
        Config::default()
            .with_poll_interval(Duration::from_secs(2))
            .with_compare_contents(true),
    )
    .ok()
}

fn watch_device_files(watcher: &mut PollWatcher, root: &Path) {
    let Ok(device) = discover(root) else { return };
    for file in ["actual_brightness", "brightness", "max_brightness"] {
        let path = device.path.join(file);
        if path.exists()
            && let Err(error) = watcher.watch(&path, RecursiveMode::NonRecursive)
        {
            tracing::debug!(%error, path = %path.display(), "backlight file watcher unavailable");
        }
    }
}

async fn set_for_device(
    device: BacklightDevice,
    requested_percent: u8,
    fallback: Option<&Path>,
) -> Result<BrightnessState> {
    let target = raw_brightness(device.max_brightness, requested_percent);
    let direct_result = tokio::fs::write(device.path.join("brightness"), target.to_string()).await;
    if let Err(error) = direct_result {
        let program = fallback.context(format!(
            "write {}: {error}; brightnessctl fallback disabled for explicit backlight root",
            device.path.join("brightness").display()
        ))?;
        let mut command = Command::new(program);
        command.args([
            "--device",
            &device.name,
            "set",
            &target.to_string(),
            "--quiet",
        ]);
        run_fallback(command, FALLBACK_TIMEOUT).await?;
    }
    let root = device
        .path
        .parent()
        .context("backlight device has no parent")?;
    Ok(discover(root)?.state())
}

async fn run_fallback(mut command: Command, deadline: Duration) -> Result<()> {
    // Dropping a timed-out/cancelled output future must also stop the helper.
    command.kill_on_drop(true);
    let output = timeout(deadline, command.output())
        .await
        .context("brightnessctl timed out")?
        .context("start brightnessctl")?;
    if !output.status.success() {
        bail!(
            "brightnessctl failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
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
mod tests;
