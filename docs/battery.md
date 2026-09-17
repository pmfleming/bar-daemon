# ThinkPad battery module

The native battery module replaces UPower for bar-daemon's laptop use case. It reads the Linux power-supply ABI directly, listens for udev changes, and keeps a 30-second recovery poll. This removes a desktop-service dependency while exposing ThinkPad features that UPower does not standardize, especially charge thresholds.

## State and behavior

`battery.changed` and `bar.snapshot` include:

- aggregate charge, charging state, external-power state, power rate, time estimates, health, and cycle count;
- lightweight history metadata under `history`; full seven-day charge/power/time-to-full samples are fetched on demand with `battery.history` to keep frequent state events small;
- every present battery under `devices`, with energy and voltage data when the kernel provides it;
- observed threshold and `charge_behaviour` capabilities;
- desired protection and alert policy separately from observed hardware values, including whether bar-daemon has taken policy ownership; and
- durable pause/calibration operation state, `charge_once_active`, and non-fatal policy/helper errors.

Multiple batteries are aggregated by energy. If every present battery lacks compatible energy values, the daemon falls back to the mean reported capacity. External power is determined from the `online` attribute of non-battery supplies rather than inferred from battery status.

The aggregate `forecast` drives Shelllist's combined charge/power timeline. While
unplugged and not charging, `target: 0` and `seconds` use the reported
`time_to_empty_seconds`; a missing estimate or one beyond seven days is marked
`estimating` with no projected line. While charging, the target is full or the
active protection limit (ignored during charge-once), and the bounded full-charge
estimate is scaled to that target. Plugging in while holding/inhibiting charge
does not retain a discharge forecast. Reaching a protection limit reports
`limit-reached` only on AC; an unplugged battery above that limit still forecasts
depletion. All valid forecasts are approximate. Historical `power_watts` samples
are battery flow, not whole-machine AC consumption; `power_valid` distinguishes
an observed zero from an unavailable reading. Existing Wh history bins remain
available to API consumers.

History metadata includes a lightweight `current_point` on every observation,
using the same active-time origin as the stored points. Clients can append it
when it is newer than their last stored point to align the live charge marker
and forecast. It is not persisted between 15-minute buckets, so live plotting
does not add disk writes or require repeated full-history requests.

The defaults are low at 25%, critical at 12%, notifications and Power saver at both levels, full notification enabled, and a suggested protected range of 75–80%. A fresh installation does not take ownership of or change existing firmware thresholds. `battery.setThresholds` stores the desired range without enabling protection or taking ownership; when protection is already managed and enabled, it also updates the hardware. Threshold management starts when `battery.setProtection` succeeds.

## Battery levels & actions

Shelllist places the shared policy under **Power & sleep → Battery levels & actions**. Each level has an editable percentage, an independent notification switch, and a profile action: `keep-current`, `power-saver`, `balanced`, or `performance`. Only profiles advertised by the system service can be newly selected. Battery care retains charging protection, health, and the full/charge-limit notification.

- Rules apply only while unplugged. Critical settings take priority, including `keep-current` (no override at that level, rather than inheriting the low-level action).
- Levels activate at or below their percentage. Recovery requires exceeding that percentage **plus 3 percentage points**; for example, low at 25% clears above 28%. Plugging in clears the episode immediately.
- Notifications fire once per crossing, rearming after recovery or AC power. A jump past both thresholds emits only the critical notification. Disabling a notification does not disable its profile action; enabling it again within the same episode does not issue a delayed duplicate.
- On startup, the appropriate profile action is applied without repeating existing low/critical alerts. The level and manual pause survive daemon restarts.
- Saver and Performance use cooperative Power Profiles holds. At recovery or on AC, only our hold is released, returning control to the previous manual selection and other applications' holds. Identical actions at both levels do not create duplicate holds.
- Power Profiles does not support a Balanced hold. For Balanced, bar-daemon durably remembers the preceding profile before selecting it. Selection/restoration waits while any other application has a hold; it never clears another application's holds to enforce Balanced. Restoration survives daemon restarts. A subsequent manual selection supersedes the remembered profile.
- Manual profile selection in the panel pauses automation until AC power or recovery above the low threshold plus its margin. **Resume automatic switching** (`powerProfile.resumeAutomatic`) clears the pause immediately. External selections that release our hold or replace a Balanced override are also respected.

`power_profile.battery_automation` exposes `level` (`normal`, `low`, `critical`), requested `profile`, `status` (`waiting`, `active`, `paused`, `keep-current`, `blocked`, `unavailable`, `error`), and an optional `error`. `active` means our request is effective; `blocked` means another application's request is taking precedence or preventing Balanced restoration. No automatic suspend or hibernate action is added.

**Adaptive hardware tuning** is separate: it controls Power Profiles' optional `BatteryAware` capability, allowing supported drivers/actions to respond to battery/AC state. It has no user-defined percentage thresholds. Hardware actions such as trickle charging are not battery charge limits.

### Migration

Legacy `warning_percent`, `critical_percent`, and `notify_when_full` are preserved. Missing per-level notification switches default to enabled. Missing profile actions inherit the old `auto_power_saver`: enabled becomes Power saver at both levels; disabled becomes Keep current. Explicit per-level actions take precedence. The deprecated `auto_power_saver` API input remains supported and sets both actions together, without changing notifications. Its output is a compatibility summary indicating that at least one level has a profile action; new clients should use the explicit actions.

## API examples

Send these records to `bar-daemon client`:

```json
{"op":"call","id":"history","method":"battery.history","params":{}}
{"op":"call","id":"thresholds","method":"battery.setThresholds","params":{"battery_id":"BAT0","start_percent":75,"end_percent":80}}
{"op":"call","id":"protect-off","method":"battery.setProtection","params":{"enabled":false}}
{"op":"call","id":"protect-on","method":"battery.setProtection","params":{"battery_id":"BAT0","enabled":true,"start_percent":75,"end_percent":80}}
{"op":"call","id":"once","method":"battery.chargeOnce","params":{}}
{"op":"call","id":"pause","method":"battery.setChargingInhibited","params":{"battery_id":"BAT0","enabled":true}}
{"op":"call","id":"resume","method":"battery.setChargingInhibited","params":{"battery_id":"BAT0","enabled":false}}
{"op":"call","id":"calibrate","method":"battery.startCalibration","params":{"battery_id":"BAT0"}}
{"op":"call","id":"cancel-calibration","method":"battery.cancelCalibration","params":{"battery_id":"BAT0"}}
{"op":"call","id":"levels","method":"battery.setAlertPolicy","params":{"warning_percent":25,"critical_percent":12,"notify_warning":true,"notify_critical":true,"warning_profile":"power-saver","critical_profile":"power-saver"}}
{"op":"call","id":"charge-notification","method":"battery.setAlertPolicy","params":{"notify_when_full":true}}
{"op":"call","id":"resume-profiles","method":"powerProfile.resumeAutomatic","params":{}}
```

`battery.setProtection` and `battery.chargeOnce` use the primary battery exposed in the aggregate state when `battery_id` is omitted. `battery.setProtection` accepts an optional complete `start_percent`/`end_percent` pair so clients can update the desired range and enabled state atomically. `battery.setThresholds` requires an explicit battery ID and preserves the existing enabled and management state. Threshold changes fail cleanly when the kernel does not expose both `charge_control_start_threshold` and `charge_control_end_threshold`.

Charge-once requires external power. Before selecting `0–100`, the daemon durably records the observed range. The operation survives daemon restarts and restores that exact range when the battery reaches 100%, external power is removed, or 24 hours elapse. The runtime marker is cleared only after verified restoration.

Charging inhibition uses the kernel's advertised `inhibit-charge` behavior and remains active across daemon restarts until explicitly resumed. Calibration requires external power, readable thresholds, and `force-discharge` support. It records the exact observed range, temporarily selects `0–100`, force-discharges to 1%, charges to 100%, and then restores the recorded range. Unplugging, cancelling, or reaching the 48-hour safety deadline returns the behavior to `auto` and restores the range. Cancellation and automatic completion durably enter a `restoring` phase before changing hardware. If restoration fails or readback differs, the recovery marker is retained and restoration is retried; force-discharge is not resumed. Only one charge-once, inhibition, or calibration operation can be active at a time.

## Persistence

Policy is stored in `$XDG_CONFIG_HOME/bar-daemon/battery.json`, falling back to `~/.config/bar-daemon/battery.json`. Runtime recovery state is stored in `$XDG_STATE_HOME/bar-daemon/battery-state.json`, falling back to `~/.local/state/bar-daemon/battery-state.json`. Graph history is stored in `$XDG_STATE_HOME/bar-daemon/battery-history-v1.json`, sampled every 15 minutes plus plug/charge transitions and pruned after seven days. The history response supplies an active-only `active_time_ms` x coordinate and marks discontinuities, so suspend, shutdown, and daemon downtime do not consume graph width. New observations use Linux's suspend-excluding monotonic clock and compare it with wall time to detect short sleeps and clock jumps as well as long observation gaps. Each point is classified as `charging`, `discharging`, or `holding`; clients should split paths where `continuous` is false. Wall-clock `timestamp_ms` remains available for labels and tooltips. Policy and runtime recovery writes use a unique temporary file, sync its contents before atomic rename, and sync the parent directory afterward. Newly created state directories are also synced. Successful persistence of these records is therefore a durability barrier before hardware changes, not just an atomic replacement. Per-device entries under `devices` override the legacy top-level protection defaults, so dual-battery ThinkPads retain independent BAT0 and BAT1 ranges. Charge-once recovery records the target battery ID and never restores a range to a different battery.

Automatic profile recovery is separately stored in `$XDG_STATE_HOME/bar-daemon/battery-profile-state.json`: the current level, manual pause, and any pre-Balanced profile to restore. It uses the same atomic/durable write mechanism and does not contain charge-control operations.

The battery provider paths can be overridden for testing or unusual deployments:

- `BAR_DAEMON_BATTERY_CONFIG`
- `BAR_DAEMON_BATTERY_STATE`
- `BAR_DAEMON_BATTERY_HISTORY`

The API is the preferred way to update the files because it validates ranges and keeps desired policy synchronized with verified hardware state. An example policy file is:

```json
{
  "warning_percent": 25,
  "critical_percent": 12,
  "notify_when_full": true,
  "notify_warning": true,
  "notify_critical": true,
  "warning_profile": "power-saver",
  "critical_profile": "power-saver",
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
