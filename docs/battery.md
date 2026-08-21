# ThinkPad battery module

The native battery module replaces UPower for bar-daemon's laptop use case. It reads the Linux power-supply ABI directly, listens for udev changes, and keeps a 30-second recovery poll. This removes a desktop-service dependency while exposing ThinkPad features that UPower does not standardize, especially charge thresholds.

## State and behavior

`battery.changed` and `bar.snapshot` include:

- aggregate charge, charging state, external-power state, power rate, time estimates, health, and cycle count;
- every present battery under `devices`, with energy and voltage data when the kernel provides it;
- observed threshold and `charge_behaviour` capabilities;
- desired protection and alert policy separately from observed hardware values, including whether bar-daemon has taken policy ownership; and
- durable pause/calibration operation state, `charge_once_active`, and non-fatal policy/helper errors.

Multiple batteries are aggregated by energy. If every present battery lacks compatible energy values, the daemon falls back to the mean reported capacity. External power is determined from the `online` attribute of non-battery supplies rather than inferred from battery status.

The defaults are warning at 25%, critical at 12%, full notification enabled, automatic power saver enabled, and a suggested protected range of 75–80%. When discharging at or below the warning percentage, bar-daemon asks power-profiles-daemon for a `power-saver` hold. It releases the hold on AC power or recovery above the warning level. A manual profile selection releases the hold and is respected until that low-battery episode ends. A fresh installation does not take ownership of or change existing firmware thresholds. Threshold management starts only after `battery.setThresholds` or `battery.setProtection` succeeds.

## API examples

Send these records to `bar-daemon client`:

```json
{"op":"call","id":"thresholds","method":"battery.setThresholds","params":{"battery_id":"BAT0","start_percent":75,"end_percent":80}}
{"op":"call","id":"protect-off","method":"battery.setProtection","params":{"enabled":false}}
{"op":"call","id":"protect-on","method":"battery.setProtection","params":{"enabled":true}}
{"op":"call","id":"once","method":"battery.chargeOnce","params":{}}
{"op":"call","id":"pause","method":"battery.setChargingInhibited","params":{"battery_id":"BAT0","enabled":true}}
{"op":"call","id":"resume","method":"battery.setChargingInhibited","params":{"battery_id":"BAT0","enabled":false}}
{"op":"call","id":"calibrate","method":"battery.startCalibration","params":{"battery_id":"BAT0"}}
{"op":"call","id":"cancel-calibration","method":"battery.cancelCalibration","params":{"battery_id":"BAT0"}}
{"op":"call","id":"alerts","method":"battery.setAlertPolicy","params":{"warning_percent":25,"critical_percent":12,"notify_when_full":true,"auto_power_saver":true}}
```

`battery.setProtection` and `battery.chargeOnce` use the primary battery exposed in the aggregate state. `battery.setThresholds` accepts an explicit battery ID for machines that expose more than one battery. Threshold changes fail cleanly when the kernel does not expose both `charge_control_start_threshold` and `charge_control_end_threshold`.

Charge-once requires external power. Before selecting `0–100`, the daemon durably records the observed range. The operation survives daemon restarts and restores that exact range when the battery reaches 100%, external power is removed, or 24 hours elapse. The runtime marker is cleared only after verified restoration.

Charging inhibition uses the kernel's advertised `inhibit-charge` behavior and remains active across daemon restarts until explicitly resumed. Calibration requires external power, readable thresholds, and `force-discharge` support. It records the exact observed range, temporarily selects `0–100`, force-discharges to 1%, charges to 100%, and then restores the recorded range. Unplugging, cancelling, or reaching the 48-hour safety deadline returns the behavior to `auto` and restores the range. Only one charge-once, inhibition, or calibration operation can be active at a time.

## Persistence

Policy is stored in `$XDG_CONFIG_HOME/bar-daemon/battery.json`, falling back to `~/.config/bar-daemon/battery.json`. Runtime recovery state is stored in `$XDG_STATE_HOME/bar-daemon/battery-state.json`, falling back to `~/.local/state/bar-daemon/battery-state.json`. Writes use a temporary file and atomic rename. Per-device entries under `devices` override the legacy top-level protection defaults, so dual-battery ThinkPads retain independent BAT0 and BAT1 ranges. Charge-once recovery records the target battery ID and never restores a range to a different battery.

The paths can be overridden for testing or unusual deployments:

- `BAR_DAEMON_BATTERY_CONFIG`
- `BAR_DAEMON_BATTERY_STATE`

The API is the preferred way to update the files because it validates ranges and keeps desired policy synchronized with verified hardware state. An example policy file is:

```json
{
  "warning_percent": 25,
  "critical_percent": 12,
  "notify_when_full": true,
  "auto_power_saver": true,
  "manage_thresholds": true,
  "protection_enabled": true,
  "protected_start_percent": 75,
  "protected_end_percent": 80,
  "devices": {
    "BAT0": {
      "manage_thresholds": true,
      "protection_enabled": true,
      "protected_start_percent": 75,
      "protected_end_percent": 80,
      "accepted_reported_start_percent": 75,
      "accepted_reported_end_percent": 80
    }
  }
}
```

## Privileged helper

Monitoring is unprivileged. Threshold writes go through `org.laufan.BarBatteryHelper` on the system bus. The packaged systemd service runs the narrowly scoped helper as root; the D-Bus policy permits calls, and polkit authorizes active local sessions. The helper validates the battery ID and sysfs type, requires both threshold files, validates the range, orders writes so an intermediate value remains valid, reads back the firmware result, and attempts rollback if the second write fails. Kernel drivers may round thresholds and some ThinkPad firmware reports values differently from those requested, so a successful write with different readback is retained as an unverified result rather than retried forever. State exposes `thresholds_verified` so clients can distinguish exact readback from an accepted firmware result.

With the NixOS module, enable the package integration with:

```nix
services.bar-daemon.enable = true;
```

This installs the daemon and helper, registers their D-Bus activation files and systemd units, and enables polkit. Other distributions need to install the files under `packaging/dbus`, `packaging/systemd`, and `packaging/polkit` into their standard system locations.

## UPower migration

Native monitoring is the default. For a temporary compatibility rollback, start the daemon with:

```sh
BAR_DAEMON_BATTERY_BACKEND=upower bar-daemon daemon
```

The compatibility backend retains basic UPower telemetry and alerts, but it does not expose native device or ThinkPad protection controls. The switch is intentionally opt-in so the native path receives normal testing; it can be removed after one compatibility cycle.

The native design uses stable Linux interfaces: `/sys/class/power_supply` for state/control and udev for change notification. Direct ACPI/procfs parsing would be less portable, vendor-specific D-Bus services would add a desktop dependency, and TLP or `thinkfan` integration would make bar-daemon depend on another policy owner. Those remain reasonable alternatives when another service should own battery policy, but they are unnecessary for this ThinkPad-focused module.
