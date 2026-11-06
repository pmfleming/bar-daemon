//! Session-side helper client. The privileged service is an independent crate.
use anyhow::{Result, bail};
#[cfg(test)]
use bar_battery_helper::{BatteryHelper, ThresholdWriter};
pub(crate) use bar_battery_helper_api::{BUS_NAME, INTERFACE, OBJECT_PATH, ThresholdWriteResult};
#[cfg(test)]
use std::collections::HashMap;
use zbus::{Connection, connection};
#[cfg(test)]
const POLKIT_BUS: &str = "org.freedesktop.PolicyKit1";
#[cfg(test)]
const POLKIT_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
pub(crate) async fn set_thresholds(
    battery: &str,
    start: u8,
    end: u8,
) -> Result<ThresholdWriteResult> {
    let (actual_start_percent, actual_end_percent, verified): (u8, u8, bool) = write_call(
        battery,
        "SetThresholds",
        "SetThresholdsNonInteractive",
        &(battery, start, end),
    )
    .await?;
    Ok(ThresholdWriteResult {
        actual_start_percent,
        actual_end_percent,
        verified,
    })
}

pub(crate) async fn set_charge_behaviour(battery: &str, behaviour: &str) -> Result<String> {
    write_call(
        battery,
        "SetChargeBehaviour",
        "SetChargeBehaviourNonInteractive",
        &(battery, behaviour),
    )
    .await
}

async fn write_call<B, R>(
    battery: &str,
    interactive_method: &str,
    automatic_method: &str,
    body: &B,
) -> Result<R>
where
    B: serde::Serialize + zvariant::DynamicType,
    R: for<'d> zvariant::DynamicDeserialize<'d>,
{
    let cancellation = super::authorization::cancellation();
    if cancellation.as_ref().is_some_and(|value| *value.borrow()) {
        bail!("battery operation cancelled before helper dispatch");
    }
    // Dedicated connection: closing it cancels only this authorization's owner,
    // never the shared fast-read/system connection or another operation.
    let budget = if cancellation.is_some() {
        super::authorization::HELPER_BUDGET
    } else {
        std::time::Duration::from_secs(5)
    };
    let connection = connection::Builder::system()?
        .method_timeout(budget)
        .build()
        .await?;
    let method = if cancellation.is_some() {
        interactive_method
    } else {
        automatic_method
    };
    let result = dispatch_write(connection, method, body, cancellation).await;
    if let Err(error) = &result
        && super::authorization::is_unknown(error)
    {
        // Caller holds the effects lock. Persist before releasing it so the
        // monitor cannot automatically replay an uncertain start.
        let mut runtime = super::config::load_runtime().await.map_err(|failure| {
            anyhow::Error::from(super::authorization::OutcomeUnknown).context(failure)
        })?;
        if !runtime
            .authorization_uncertain_batteries
            .iter()
            .any(|id| id == battery)
        {
            runtime
                .authorization_uncertain_batteries
                .push(battery.into());
        }
        super::config::save_runtime(&runtime)
            .await
            .map_err(|failure| {
                anyhow::Error::from(super::authorization::OutcomeUnknown).context(failure)
            })?;
    }
    result
}

async fn dispatch_write<B, R>(
    connection: Connection,
    method: &str,
    body: &B,
    mut cancellation: Option<tokio::sync::watch::Receiver<bool>>,
) -> Result<R>
where
    B: serde::Serialize + zvariant::DynamicType,
    R: for<'d> zvariant::DynamicDeserialize<'d>,
{
    let proxy = zbus::Proxy::new(&connection, BUS_NAME, OBJECT_PATH, INTERFACE).await?;
    let call = proxy.call(method, body);
    let result = if let Some(receiver) = cancellation.as_mut() {
        tokio::select! {
            biased;
            _ = super::authorization::cancelled(receiver) => None,
            result = call => Some(result),
        }
    } else {
        Some(call.await)
    };
    drop(proxy);
    // Explicit close, rather than relying on connection clone drop semantics.
    let _ = connection.close().await;
    match result {
        Some(Ok(value)) => Ok(value),
        Some(Err(zbus::Error::MethodError(name, message, _)))
            if name.as_str() == "org.freedesktop.DBus.Error.AccessDenied" =>
        {
            bail!(
                "{}",
                message.unwrap_or_else(|| "authorization denied".into())
            )
        }
        // Failed replies can include partial writes; never imply rollback.
        _ => Err(super::authorization::OutcomeUnknown.into()),
    }
}

#[cfg(test)]
#[path = "authorization_tests.rs"]
pub(crate) mod authorization_tests;

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::ThresholdWriter;

    pub(super) fn writer(start: u8, end: u8) -> (TempDir, ThresholdWriter) {
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

    #[tokio::test]
    async fn threshold_contract_uses_actual_isolated_helper_readback() {
        let (_directory, writer) = writer(75, 80);
        assert_eq!(writer.get_charge_behaviour("BAT0").unwrap().0, "auto");
        assert!(writer.set_charge_behaviour("BAT0", "ship-mode").is_err());
        assert!(writer.set_thresholds("../../etc", 60, 80).is_err());
        assert!(writer.set_thresholds("BAT0", 80, 80).is_err());
        let result = writer.set_thresholds("BAT0", 60, 80).unwrap();
        let fixture = crate::protocol::battery_contract_fixture().await;
        let expected = &fixture["cases"]["battery.setThresholds"]["success"];
        let state = serde_json::from_value(expected["data"]["battery"].clone()).unwrap();
        assert_eq!(
            crate::protocol::battery_wire::response(Ok(state), Some(("BAT0", result))),
            *expected
        );
        assert_eq!(writer.get_thresholds("BAT0").unwrap(), (60, 80));
    }
}
