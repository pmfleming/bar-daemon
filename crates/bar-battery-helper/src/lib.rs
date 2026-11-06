//! Privileged battery and runtime hibernation service.
mod runtime;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tokio::signal::{
    ctrl_c,
    unix::{SignalKind, signal},
};
use zbus::{Connection, connection, message::Header};

use bar_battery_helper_api::{
    AUTHORIZATION_BUDGET, HELPER_BUDGET, ThresholdWriteResult, valid_thresholds,
    validate_battery_id,
};
pub use bar_battery_helper_api::{BUS_NAME, INTERFACE, OBJECT_PATH};

const POLKIT_THRESHOLDS_ACTION: &str = "org.laufan.bar-daemon.set-battery-thresholds";
const POLKIT_BEHAVIOUR_ACTION: &str = "org.laufan.bar-daemon.set-battery-charge-behaviour";

pub struct BatteryHelper {
    writer: ThresholdWriter,
}

impl BatteryHelper {
    pub fn new(writer: ThresholdWriter) -> Self {
        Self { writer }
    }
}

impl Default for BatteryHelper {
    fn default() -> Self {
        Self {
            writer: ThresholdWriter::system(),
        }
    }
}

#[zbus::interface(name = "org.laufan.BarBatteryHelper1")]
impl BatteryHelper {
    async fn sleep_settings_available(&self) -> bool {
        true
    }

    async fn can_set_hibernate_delay(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> bool {
        authorize(
            connection,
            &header,
            "org.laufan.bar-daemon.set-hibernate-delay",
            0,
        )
        .await
        .is_ok()
    }

    async fn cleanup_hibernate_delay(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<bool> {
        authorize(
            connection,
            &header,
            "org.laufan.bar-daemon.set-hibernate-delay",
            0,
        )
        .await?;
        crate::runtime::cleanup().await.map_err(failed)
    }

    async fn acquire_hibernate_delay(
        &self,
        minutes: u32,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<zvariant::OwnedFd> {
        // An idle callback must never launch an authentication prompt.
        authorize(
            connection,
            &header,
            "org.laufan.bar-daemon.set-hibernate-delay",
            0,
        )
        .await?;
        crate::runtime::acquire(minutes).await.map_err(failed)
    }

    async fn get_thresholds(&self, battery: &str) -> zbus::fdo::Result<(u8, u8)> {
        self.writer.get_thresholds(battery).map_err(failed)
    }

    async fn set_thresholds(
        &self,
        battery: &str,
        start_percent: u8,
        end_percent: u8,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<(u8, u8, bool)> {
        authorize(connection, &header, POLKIT_THRESHOLDS_ACTION, 1).await?;
        let result = self
            .writer
            .set_thresholds(battery, start_percent, end_percent)
            .map_err(failed)?;
        Ok((
            result.actual_start_percent,
            result.actual_end_percent,
            result.verified,
        ))
    }

    async fn set_thresholds_non_interactive(
        &self,
        battery: &str,
        start_percent: u8,
        end_percent: u8,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<(u8, u8, bool)> {
        authorize(connection, &header, POLKIT_THRESHOLDS_ACTION, 0).await?;
        let result = self
            .writer
            .set_thresholds(battery, start_percent, end_percent)
            .map_err(failed)?;
        Ok((
            result.actual_start_percent,
            result.actual_end_percent,
            result.verified,
        ))
    }

    async fn set_charge_behaviour_non_interactive(
        &self,
        battery: &str,
        behaviour: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<String> {
        authorize(connection, &header, POLKIT_BEHAVIOUR_ACTION, 0).await?;
        self.writer
            .set_charge_behaviour(battery, behaviour)
            .map_err(failed)
    }

    async fn get_charge_behaviour(
        &self,
        battery: &str,
    ) -> zbus::fdo::Result<(String, Vec<String>)> {
        self.writer.get_charge_behaviour(battery).map_err(failed)
    }

    async fn set_charge_behaviour(
        &self,
        battery: &str,
        behaviour: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<String> {
        authorize(connection, &header, POLKIT_BEHAVIOUR_ACTION, 1).await?;
        self.writer
            .set_charge_behaviour(battery, behaviour)
            .map_err(failed)
    }
}

fn failed(error: anyhow::Error) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(error.to_string())
}

async fn authorize(
    connection: &Connection,
    header: &Header<'_>,
    action: &str,
    flags: u32,
) -> zbus::fdo::Result<()> {
    let sender = header
        .sender()
        .ok_or_else(|| zbus::fdo::Error::AccessDenied("caller has no D-Bus identity".into()))?;
    let budget = if flags == 0 {
        std::time::Duration::from_secs(3)
    } else {
        AUTHORIZATION_BUDGET
    };
    shelllist_daemon_tokio::authorize_polkit(connection, sender, action, budget, flags != 0).await
}

#[derive(Debug, Clone)]
pub struct ThresholdWriter {
    root: PathBuf,
}

impl ThresholdWriter {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn system() -> Self {
        Self::new(PathBuf::from("/sys/class/power_supply"))
    }

    pub fn get_thresholds(&self, battery: &str) -> Result<(u8, u8)> {
        let directory = self.threshold_directory(battery)?;
        Ok((
            read_percent(&directory.join("charge_control_start_threshold"))?,
            read_percent(&directory.join("charge_control_end_threshold"))?,
        ))
    }

    pub fn set_thresholds(
        &self,
        battery: &str,
        start: u8,
        end: u8,
    ) -> Result<ThresholdWriteResult> {
        if !valid_thresholds(start, end) {
            bail!("thresholds must satisfy 0 <= start < end <= 100");
        }
        let directory = self.threshold_directory(battery)?;
        let start_path = directory.join("charge_control_start_threshold");
        let end_path = directory.join("charge_control_end_threshold");
        let (current_start, current_end) = (read_percent(&start_path)?, read_percent(&end_path)?);
        if (current_start, current_end) == (start, end) {
            return Ok(ThresholdWriteResult {
                actual_start_percent: start,
                actual_end_percent: end,
                verified: true,
            });
        }

        let (first_path, first_value, second_path, second_value, rollback_value) =
            if start >= current_end {
                (&end_path, end, &start_path, start, current_end)
            } else {
                (&start_path, start, &end_path, end, current_start)
            };
        write_percent(first_path, first_value)?;
        if let Err(error) = write_percent(second_path, second_value) {
            let rollback = write_percent(first_path, rollback_value);
            return match rollback {
                Ok(()) => {
                    Err(error).context("second threshold write failed; first write rolled back")
                }
                Err(rollback_error) => Err(error).context(format!(
                    "second threshold write failed and rollback also failed: {rollback_error}"
                )),
            };
        }

        let actual = self.get_thresholds(battery)?;
        Ok(ThresholdWriteResult {
            actual_start_percent: actual.0,
            actual_end_percent: actual.1,
            verified: actual == (start, end),
        })
    }

    pub fn get_charge_behaviour(&self, battery: &str) -> Result<(String, Vec<String>)> {
        let directory = self.battery_directory(battery)?;
        parse_choices(&std::fs::read_to_string(
            directory.join("charge_behaviour"),
        )?)
    }

    pub fn set_charge_behaviour(&self, battery: &str, behaviour: &str) -> Result<String> {
        if !matches!(
            behaviour,
            "auto" | "inhibit-charge" | "inhibit-charge-awake" | "force-discharge"
        ) {
            bail!("unsupported charge behaviour: {behaviour}");
        }
        let directory = self.battery_directory(battery)?;
        let path = directory.join("charge_behaviour");
        let (_, available) = parse_choices(&std::fs::read_to_string(&path)?)?;
        if !available.iter().any(|item| item == behaviour) {
            bail!("battery {battery} does not support charge behaviour {behaviour}");
        }
        std::fs::write(&path, behaviour).with_context(|| format!("write {}", path.display()))?;
        let (actual, _) = parse_choices(&std::fs::read_to_string(&path)?)?;
        if actual != behaviour {
            bail!(
                "battery {battery} reported charge behaviour {actual} after requesting {behaviour}"
            );
        }
        Ok(actual)
    }

    fn battery_directory(&self, battery: &str) -> Result<PathBuf> {
        validate_battery_id(battery)?;
        let directory = self.root.join(battery);
        let supply_type = std::fs::read_to_string(directory.join("type"))
            .with_context(|| format!("read battery type for {battery}"))?;
        if supply_type.trim() != "Battery" {
            bail!("power supply {battery} is not a battery");
        }
        Ok(directory)
    }

    fn threshold_directory(&self, battery: &str) -> Result<PathBuf> {
        let directory = self.battery_directory(battery)?;
        for attribute in [
            "charge_control_start_threshold",
            "charge_control_end_threshold",
        ] {
            if !directory.join(attribute).is_file() {
                bail!("battery {battery} does not support {attribute}");
            }
        }
        Ok(directory)
    }
}

fn parse_choices(contents: &str) -> Result<(String, Vec<String>)> {
    let choices = contents
        .split_whitespace()
        .map(|value| value.trim_matches(['[', ']']).to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let selected = contents
        .split_whitespace()
        .find(|value| value.starts_with('[') && value.ends_with(']'))
        .map(|value| value.trim_matches(['[', ']']).to_string())
        .or_else(|| (choices.len() == 1).then(|| choices[0].clone()))
        .context("charge_behaviour does not identify its selected value")?;
    Ok((selected, choices))
}

fn read_percent(path: &Path) -> Result<u8> {
    let value = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?
        .trim()
        .parse::<u8>()
        .with_context(|| format!("parse {}", path.display()))?;
    if value > 100 {
        bail!("{} contains an invalid percentage", path.display());
    }
    Ok(value)
}

fn write_percent(path: &Path, value: u8) -> Result<()> {
    std::fs::write(path, value.to_string()).with_context(|| format!("write {}", path.display()))
}

pub async fn run() -> Result<()> {
    let _connection = connection::Builder::system()
        .context("connect battery helper to system D-Bus")?
        .method_timeout(HELPER_BUDGET)
        .name(BUS_NAME)
        .context("claim battery helper bus name")?
        .serve_at(OBJECT_PATH, BatteryHelper::default())
        .context("export battery helper interface")?
        .build()
        .await
        .context("start battery helper D-Bus service")?;
    tracing::info!(
        bus_name = BUS_NAME,
        object_path = OBJECT_PATH,
        "battery helper started"
    );
    let mut terminate = signal(SignalKind::terminate()).context("listen for SIGTERM")?;
    tokio::select! {
        _ = crate::runtime::maintain() => Ok(()),
        result = ctrl_c() => result.context("wait for Ctrl-C"),
        _ = terminate.recv() => Ok(()),
    }
}
