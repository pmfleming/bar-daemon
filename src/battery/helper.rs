use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use tokio::signal::{
    ctrl_c,
    unix::{SignalKind, signal},
};
use zbus::{Connection, connection, message::Header};
use zvariant::Value;

pub(crate) const BUS_NAME: &str = "org.laufan.BarBatteryHelper";
pub(crate) const OBJECT_PATH: &str = "/org/laufan/BarBatteryHelper";
pub(crate) const INTERFACE: &str = "org.laufan.BarBatteryHelper1";
const POLKIT_BUS: &str = "org.freedesktop.PolicyKit1";
const POLKIT_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const POLKIT_INTERFACE: &str = "org.freedesktop.PolicyKit1.Authority";
const POLKIT_THRESHOLDS_ACTION: &str = "org.laufan.bar-daemon.set-battery-thresholds";
const POLKIT_BEHAVIOUR_ACTION: &str = "org.laufan.bar-daemon.set-battery-charge-behaviour";

pub(crate) struct BatteryHelper {
    writer: ThresholdWriter,
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
        authorize(connection, &header, POLKIT_THRESHOLDS_ACTION).await?;
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
        authorize(connection, &header, POLKIT_BEHAVIOUR_ACTION).await?;
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
) -> zbus::fdo::Result<()> {
    let sender = header
        .sender()
        .ok_or_else(|| zbus::fdo::Error::AccessDenied("caller has no D-Bus identity".into()))?;
    let proxy = zbus::Proxy::new(connection, POLKIT_BUS, POLKIT_PATH, POLKIT_INTERFACE)
        .await
        .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
    let subject_details = HashMap::from([("name", Value::from(sender.as_str()))]);
    let subject = ("system-bus-name", subject_details);
    let details = HashMap::<&str, &str>::new();
    let (authorized, _challenge, _details): (bool, bool, HashMap<String, String>) = proxy
        .call("CheckAuthorization", &(subject, action, details, 1u32, ""))
        .await
        .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
    if authorized {
        Ok(())
    } else {
        Err(zbus::fdo::Error::AccessDenied(
            "battery threshold change was not authorized".into(),
        ))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ThresholdWriter {
    root: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThresholdWriteResult {
    pub actual_start_percent: u8,
    pub actual_end_percent: u8,
    pub verified: bool,
}

impl ThresholdWriter {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn system() -> Self {
        Self::new(PathBuf::from("/sys/class/power_supply"))
    }

    pub(crate) fn get_thresholds(&self, battery: &str) -> Result<(u8, u8)> {
        let directory = self.threshold_directory(battery)?;
        Ok((
            read_percent(&directory.join("charge_control_start_threshold"))?,
            read_percent(&directory.join("charge_control_end_threshold"))?,
        ))
    }

    pub(crate) fn set_thresholds(
        &self,
        battery: &str,
        start: u8,
        end: u8,
    ) -> Result<ThresholdWriteResult> {
        if start >= end || end > 100 {
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

    pub(crate) fn get_charge_behaviour(&self, battery: &str) -> Result<(String, Vec<String>)> {
        let directory = self.battery_directory(battery)?;
        parse_choices(&std::fs::read_to_string(
            directory.join("charge_behaviour"),
        )?)
    }

    pub(crate) fn set_charge_behaviour(&self, battery: &str, behaviour: &str) -> Result<String> {
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

fn validate_battery_id(battery: &str) -> Result<()> {
    let suffix = battery
        .strip_prefix("BAT")
        .context("battery id must start with BAT")?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("battery id must match BAT followed by digits");
    }
    Ok(())
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

pub(crate) async fn run() -> Result<()> {
    let _connection = connection::Builder::system()
        .context("connect battery helper to system D-Bus")?
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
        result = ctrl_c() => result.context("wait for Ctrl-C"),
        _ = terminate.recv() => Ok(()),
    }
}

pub(crate) async fn set_thresholds(
    battery: &str,
    start: u8,
    end: u8,
) -> Result<ThresholdWriteResult> {
    let connection = Connection::system()
        .await
        .context("connect to system D-Bus")?;
    let proxy = zbus::Proxy::new(&connection, BUS_NAME, OBJECT_PATH, INTERFACE)
        .await
        .context("connect to battery helper")?;
    let (actual_start_percent, actual_end_percent, verified): (u8, u8, bool) = proxy
        .call("SetThresholds", &(battery, start, end))
        .await
        .context("set battery thresholds")?;
    Ok(ThresholdWriteResult {
        actual_start_percent,
        actual_end_percent,
        verified,
    })
}

pub(crate) async fn set_charge_behaviour(battery: &str, behaviour: &str) -> Result<String> {
    let connection = Connection::system()
        .await
        .context("connect to system D-Bus")?;
    let proxy = zbus::Proxy::new(&connection, BUS_NAME, OBJECT_PATH, INTERFACE)
        .await
        .context("connect to battery helper")?;
    proxy
        .call("SetChargeBehaviour", &(battery, behaviour))
        .await
        .context("set battery charge behaviour")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::ThresholdWriter;

    fn writer(start: u8, end: u8) -> (TempDir, ThresholdWriter) {
        let directory = TempDir::new().unwrap();
        let battery = directory.path().join("BAT0");
        fs::create_dir(&battery).unwrap();
        fs::write(battery.join("type"), "Battery\n").unwrap();
        fs::write(
            battery.join("charge_control_start_threshold"),
            start.to_string(),
        )
        .unwrap();
        fs::write(
            battery.join("charge_behaviour"),
            "[auto] inhibit-charge force-discharge",
        )
        .unwrap();
        fs::write(
            battery.join("charge_control_end_threshold"),
            end.to_string(),
        )
        .unwrap();
        let root = directory.path().to_path_buf();
        (directory, ThresholdWriter::new(root))
    }

    #[test]
    fn validates_targets_and_thresholds() {
        let (_directory, writer) = writer(75, 80);
        assert!(writer.set_thresholds("../../etc", 60, 80).is_err());
        assert!(writer.set_thresholds("BAT0", 80, 80).is_err());
        assert_eq!(
            writer.set_thresholds("BAT0", 60, 80).unwrap(),
            super::ThresholdWriteResult {
                actual_start_percent: 60,
                actual_end_percent: 80,
                verified: true,
            }
        );
    }

    #[test]
    fn raises_end_before_start_when_ranges_do_not_overlap() {
        let (_directory, writer) = writer(40, 60);
        assert!(writer.set_thresholds("BAT0", 75, 80).unwrap().verified);
    }

    #[test]
    fn validates_charge_behaviour_choices() {
        let (_directory, writer) = writer(75, 80);
        assert_eq!(
            writer.get_charge_behaviour("BAT0").unwrap(),
            (
                "auto".into(),
                vec![
                    "auto".into(),
                    "inhibit-charge".into(),
                    "force-discharge".into()
                ]
            )
        );
        assert!(writer.set_charge_behaviour("BAT0", "ship-mode").is_err());
    }
}
