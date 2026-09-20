# Power and sleep

The `power_sleep` snapshot domain and `power-sleep.changed` stream are backed by systemd-logind. They expose the exact `CanSuspend` and `CanHibernate` capability values (`yes`, `no`, `challenge`, `na`, `inhibited`, `inhibitor-blocked`, or `challenge-inhibitor-blocked`), the live `PrepareForSleep` state, and every current logind inhibitor with its owner, reason, mode, UID, and PID. The additive `diagnostics` object reports read-only kernel/swap evidence and ThinkPad guidance; it does not override logind's authorization or capability decisions.

The API provides `powerSleep.lock`, `powerSleep.suspend`, and `powerSleep.hibernate`. It resolves logind's current-session alias to a **concrete session object** and requests `Lock` only if not already confirmed locked. Lock completion has a five-second deadline. On Hyprland, a per-operation, read-only `hyprland-lock-notify-v1` connection supplies confirmation: its `locked` event follows the compositor's lock-screen transition barrier. Registry discovery and each subsequent protocol synchronization are bounded to two seconds. Already-locked sessions work, and unlock/disconnect events invalidate confirmation. No `hyprctl dispatch`, process-presence checks, input synthesis or manufactured `LockedHint` are used. In other desktop sessions, the existing logind `LockedHint` acknowledgement is required instead.

**Why the native Hyprland observer matters:** hypridle 0.1.7 receives compositor lock notifications but does not itself publish `SetLockedHint`. Waiting only for that hint can therefore fail on a correctly locked Hyprland/hyprlock session. The observer uses the actual completion event, not the earlier “lock requested” state. Hyprland detection uses `HYPRLAND_INSTANCE_SIGNATURE` or `XDG_CURRENT_DESKTOP`; the resident service must inherit the matching `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR`. A missing protocol or disconnected compositor fails closed rather than falling back to stale hints. Run one resident service for the primary graphical session.

Before sleep, the pinned session must still be active and locked, and logind must confirm it is not already preparing for sleep. These checks run again after any privileged setup; unreadable preparation/lock state is an error, not `false`. Failed locking never changes hibernate settings. Once logind accepts an action, a subsequent telemetry failure is returned as degraded status, **not** a failed operation inviting another sleep request. Suspend and hibernate remain non-interactive logind requests with no inhibitor bypass. Only `yes` is executable without interaction. `challenge`, inhibition values, denial and unknown values are rejected before locking and explained in the UI. Policy configuration remains available during temporary inhibition or missing authorization; `sleep_policy.hibernate_available` describes support, `hibernate_ready` describes current non-interactive readiness (including the privileged helper), and `hibernate_error` explains restrictions.

Hypridle remains the owner of idle detection and inhibitor handling. The optional managed integration below supplies its sleep timeout; bar-daemon does not add an independent inactivity timer.

## Keep awake

The coffee-cup toggle beside Lock/Suspend/Hibernate calls `powerSleep.setKeepAwake`
with `{"enabled": true}` (or `false`). The resident daemon holds a logind
**sleep:handle-lid-switch / block** inhibitor FD; closing the panel does not
release it. The low-level lid inhibitor is necessary because logind normally
uses `LidSwitchIgnoreInhibited=yes`. No `idle` inhibitor is taken: automatic
locking and screen blanking continue, as does explicit Lock. The saved sleep
profiles and hypridle configuration are not changed.

`power_sleep.keep_awake` reports this daemon's inhibitor from logind's live list,
not an optimistic frontend flag. Shelllist disables its manual sleep buttons
while active. Manual, idle and managed-lid sleep paths also reject it before
locking or changing hibernate settings, and recheck before the logind request.
Toggle changes share the sleep-action guard. Enabling during sleep preparation
fails; acquisition/permission failures never claim success. Disabling closes
the FD even if status telemetry is unavailable. Other applications' inhibitors
are never released. Old daemons show the new button disabled with an upgrade
explanation; both shelllist and bar-daemon must be rebuilt for the feature.

This is temporary, not persisted: turning it off, daemon exit/restart, reboot,
or logind restart ends protection. After logind restart, the live list reports
it off; enabling again reacquires the FD. It does not prevent shutdown, forced
privileged sleep/inhibitor bypass, or hardware battery exhaustion. Keep awake
can therefore increase battery drain; do not rely on it as battery protection.

## Laptop docking and external-only displays

With Home Manager's `programs.shelllist.displays.enable = true`, the resident
bar-daemon owns laptop-panel switching. Nix only opts into ownership through
`BAR_DAEMON_DISPLAY_CONTROL=1`; the **Prefer external** preference
in Shelllist's dedicated Displays callout is persisted in `$XDG_CONFIG_HOME/bar-daemon/displays.json`.
`displayPolicy.set` accepts only `{"prefer_external": true|false}`. The additive
`display_policy` snapshot and `display-policy.changed` stream expose the saved
preference, integration availability, recovery status and errors. The preference
defaults to true when the integration is enabled; without the opt-in, the daemon
never changes displays. Turning the preference off enables the internal panel
alongside external displays. Settings can be saved even while the compositor is
unavailable; the daemon reconciles them when the active session returns.

On startup, wake, output loss or replacement, an internal eDP/LVDS/DSI panel is
kept as a fallback until the same enabled, nonzero-size external output topology
has been observed for five seconds. Polling every two seconds is independent of
compositor event traffic. Resume generation resets stability; sleep preparation
pauses changes. Before disabling a panel, the daemon rechecks external topology,
active local Wayland-session ownership, and sleep generation. Failed commands
remain retryable. DPMS-off is **not** output loss, so idle blanking is preserved.
Existing external modes/positions are left alone, avoiding a second mode change
during hotplug. Internal fallback uses its reported scale and preferred mode.
No saved Lua file is executed, no shell command is spawned, and no lock or lid
sleep policy is changed. Only validated compositor-reported internal connector
names can be passed to the fixed native Hyprland IPC operation.

**Migration:** remove the old `hypr-monitor-auto` script, package and user service
when enabling this integration. The managed daemon unit conflicts with and is
ordered after that legacy unit, and the runtime also refuses display mutations
while it is still active. Do not run another automatic display manager alongside
this policy. Activate the updated daemon, UI and Home Manager wiring together;
source changes alone do not replace a currently running legacy service.

This improves display-policy recovery, not GPU/USB-C firmware recovery. A cable
being connected or logind reporting a successful resume does not prove the
external display is usable. The output can still disappear after the final
check; the next reconciliation restores the laptop fallback when Hyprland can
control it. No forced GPU resets, DPMS wake loops, or sleep-inhibitor bypasses
are attempted.

## Reliability and critical-battery protection

See [Sleep reliability follow-up](sleep-reliability.md) for bounded action/lid
coordination, live idle-episode cancellation, operation outcome tracking, and
runtime override ownership/cleanup. `power_sleep.operation` distinguishes
accepted requests from completed or failed systemd jobs and unknown outcomes.

Optional awake critical-battery protection is **disabled by default**. Configure
it in Battery & Power or through `powerSleep.setCriticalPolicy` with
`{"enabled":true,"percent":5,"grace_seconds":60}`. A delivered warning precedes
one cancellable, locked, inhibitor-respecting Hibernate attempt. AC/recovery
cancels the countdown; `powerSleep.cancelCritical` cancels the episode. Attempts
are durably latched across daemon restarts, with no automatic retry or shutdown
fallback. Settings/cancellation remain usable when idle integration is unavailable.
Use only with working hibernation and no competing critical-battery power manager.
This does not wake ordinary suspended RAM to save it; combined systemd sleep
remains the owner of asleep battery protection.

## Automatic sleep, then hibernate

The `sleep_policy` snapshot domain and `sleep-policy.changed` stream expose the persisted policy, active profile (`shared`, `battery`, or `plugged`), integration availability, hibernation availability/reason, and last automatic-action failure. `powerSleep.setPolicy` accepts the complete policy:

```json
{"lid_action": "profile", "same_profile": false, "battery": {"sleep_minutes": 15, "hibernate_minutes": 60}, "plugged": {"sleep_minutes": 45, "hibernate_minutes": 180}}
```

Both fields are whole minutes, 0–10080. Zero means Never. `sleep_minutes` is inactivity before suspend; `hibernate_minutes` is **additional time asleep**, not time since last input. Never sleep disables automatic sleep; Never hibernate uses ordinary suspend. Shared mode uses the battery profile and preserves the plugged profile for later reuse. Initial settings are shared, 30 minutes to sleep, Never hibernate. Policies are atomically persisted under `$XDG_CONFIG_HOME/bar-daemon/sleep.json`. Invalid persisted data fails visibly instead of silently adopting defaults.

### Lid close

`lid_action` accepts `system` (default for existing policies), `ignore`, `lock`, `suspend`, `hibernate`, or `profile`. Profile locks, then uses the current AC/battery profile's additional hibernate delay; zero means ordinary suspend. It works even when inactivity sleep is Never. Manual Suspend is unchanged.

With managed integration enabled, the resident daemon takes logind's **handle-lid-switch** block inhibitor only for a non-System policy in its active, local graphical session. It does not take a sleep-block inhibitor or bypass other applications' sleep inhibitors. Docked/external-display use is ignored. Only a new open→closed transition triggers an action; startup, reconnect, undocking, and session activation with the lid already closed do not. The lid and session are rechecked after lock/setup. Failures are exposed in `sleep_policy.lid.error`; there is no automatic retry.

Selecting System, disabling integration, stopping the daemon, or losing its connection releases the FD and restores the administrator's logind policy. This is a deliberate fallback, not a system-wide logind configuration rewrite. The snapshot's `lid.available` and `lid.managed` distinguish configured policy from current ownership. No sleep cycle is initiated by saving settings.

### Integration and ownership

Shelllist's Home Manager option `programs.shelllist.sleep.enable` replaces `hypridle.service`'s executable with:

```sh
bar-daemon idle --config "$HOME/.config/hypr/hypridle.conf" --hypridle /path/to/hypridle
```

The resident daemon gets `BAR_DAEMON_IDLE_CONFIG` pointing at that same base config. The wrapper preserves lock/DPMS listeners and logind/Wayland inhibitor settings, removes only simple two-setting `systemctl`/`loginctl` suspend/hibernate listeners, writes `$XDG_RUNTIME_DIR/bar-daemon/hypridle.conf`, then execs hypridle. Source includes, custom sleep command chains, and sleep listeners with additional resume/other settings are rejected: move non-sleep behavior into separate listeners before enabling integration. Undetectable custom scripts that sleep must also be removed manually. The declarative base file is never modified. If UPower or the saved policy cannot be read, the wrapper retains lock/DPMS listeners with automatic sleep disabled. If the base itself cannot be safely migrated, it runs the original configuration unchanged and the controls remain unavailable. Disabling the option restores the original hypridle service and policy.

The updated Home Manager integration runs a readiness-patched hypridle under `Type=notify`, with an eight-second startup deadline. Managed parser/listener errors fail startup instead of silently ignoring entries. Native READY is sent only after Wayland listener synchronization and D-Bus/sleep-inhibitor initialization. A generated config is never readiness: the daemon also verifies the notify unit is running, its MainPID matches the generation, and that it remains the same running generation for a short stabilization interval. Old `Type=simple` integration is reported unavailable until Home Manager is activated. AC lookup is bounded to two seconds so a missing UPower cannot consume the entire startup window before the lock/DPMS-only fallback starts.

Saving settings updates Hypridle's native managed sleep listener in place; a failed update restores the previous policy and attempts to restore its live timeout. AC changes update only that listener when the inactivity delay changes. The Hypridle process, screensaver inhibitor cookies, and lock/DPMS countdowns are preserved. Lid-only and hibernate-delay-only changes do not reset any listener. The live `org.laufan.Hypridle1` control interface is required; old integrations fail visibly instead of falling back to disruptive restarts. The wrapper and idle callback both read UPower's current AC state rather than relying on stale UI telemetry. `powerSleep.idle` takes `sleep_minutes`, a live process/countdown `generation`, and an `episode` token from the native listener. Renewed activity or inhibition invalidates the episode independently of the daemon's policy mutex; live episode validation runs again after final preflight queries. It refuses callbacks from a restarted countdown, a different timeout, or a Never profile. After lock confirmation and again after hibernate-helper setup, it rereads the current power source, policy and countdown generation. A changed timeout or hibernate delay cancels the request rather than sleeping with the old profile; an equivalent shared profile remains valid. Sleep failures remain visible in `last_error` until a successful action or policy save. No action is automatically retried.

When hibernation is configured, the callback enters the same action guard as manual sleep, checks `CanSuspendThenHibernate`, confirms locking, and **only then** asks the privileged helper to set a bounded runtime-only hibernate delay. It re-checks active-session, lock and preparation state before invoking logind's `SuspendThenHibernate(false)`. There is no awake-process timer trying to count time while suspended. Systemd owns waking to hibernate, cancellation on normal wake, and protective early hibernation at critically low battery. Profile selection happens at sleep entry; changing AC power while asleep does not select a new delay for that cycle. Manual Suspend remains ordinary suspend and does not pick up this automatic policy.

The helper's polkit-gated `AcquireHibernateDelay(u32 minutes)` leases `/run/systemd/sleep.conf.d/90-shelllist.conf` through an FD, containing `HibernateDelaySec` and `HibernateOnACPower=yes`. It accepts no path, command or arbitrary config text; an idle callback cannot prompt. Effective settings are read back with `systemd-analyze cat-config`; a conflicting administrator drop-in cancels setup instead of silently using a different delay. The override remains system-wide and can affect other callers while leased. The helper refuses administrator-owned contents, serializes leases, and removes only its exact unchanged generated file after release and verified absence of sleep jobs. A 30-second settling interval protects in-flight dispatch; abandoned leases are checked after 120 awake seconds. Bus uncertainty defers cleanup. Helper maintenance also cleans orphaned/legacy generated settings after restart; disabling managed hibernation requests cleanup without disturbing a live cycle. No swap provisioning, resume-device changes, bootloader changes or logind inhibitor bypasses are performed. Hibernation support and the updated system helper must be installed separately from a Home Manager-only UI update.

## Lenovo/ThinkPad diagnostics and remaining setup

Read-only checks (they never lock, suspend or hibernate):

```sh
bar-daemon debug sleep-diagnostics
bar-daemon debug lock-state
```

The reviewed host is a **Lenovo ThinkPad P14s Gen 5 AMD (21ME)**. Its kernel exposes `[s2idle]`, not `deep`; `amd_pmc` is loaded and bound to `AMDI0009:00`. Do not force `mem_sleep_default=deep`, write firmware/EC registers, unload drivers, or blanket-disable wake sources. For excessive sleep drain, first check Lenovo BIOS/EC updates and current kernel fixes, then measure AMD PMC/S0ix residency and inspect wake sources during a deliberately scheduled suspend/resume test. None of those hardware state changes or sleep cycles are performed by the diagnostics command.

The original review found only zram. A later read-only check found an active **64-GiB `/swapfile`**, and logind advertised both hibernate and suspend-then-hibernate as supported. These checks are not proof of successful end-to-end resume. Zram itself is volatile and cannot hold a hibernation image; persistent swap and working boot/initrd resume support remain required. Swapfile setup is filesystem-specific (including Btrfs allocation rules and resume offsets), so no automatic swap allocation or bootloader changes are attempted. Conversely, `resume=0:0` alone does **not** prove hibernation impossible: modern systemd can discover persistent swap and record the target through EFI. Missing/unreadable evidence stays unknown. Kernel lockdown is reported as a possible policy restriction, never automatically disabled.

The per-profile delay continues to use systemd's suspend-then-hibernate and kernel wake timers, not a Lenovo-specific replacement or a daemon timer that stops during suspend. The helper's runtime delay is system-wide and remains subject to administrator drop-in precedence. End-to-end firmware wake/resume and hibernation reliability still need a controlled hardware test after persistent swap/resume is configured. The automated tests use isolated logind and Wayland protocol servers.

## Resume recovery

`power_sleep.resume_generation` increases on `PrepareForSleep(false)` independently of telemetry queries. It survives subsequent action/status refreshes and lets a frontend detect resume even if transient preparation states were coalesced. A two-second CLOCK_BOOTTIME minus CLOCK_MONOTONIC check backs up missed logind signals without treating NTP changes or an event-loop stall as sleep. Signal and clock detections for one cycle are deduplicated. A daemon restart resets the generation; clients establish a new baseline.

Signal connection/reconnection, transition detection, telemetry refresh, and operation-outcome tracking run as four independently polled futures with one cancellation owner. Neither a stuck capability/inhibitor query nor slow system-bus setup can stall the clock detector. Preparation and resume changes are published immediately. Telemetry has a ten-second deadline and a single worker; refresh bursts coalesce rather than spawning parallel queries. A reply is committed only if the sleep state has not changed since its query began, so an old response cannot restore pre-resume preparation state or overwrite a newer Keep awake change. Superseded queries trigger a fresh read. Dropping the monitor cancels all four futures; these display-telemetry deadlines do not weaken action preflight.

Shelllist observes this generation to rebuild bar surfaces; screen-change recovery remains independent. There is no wall-clock-gap resume heuristic.

## Explicit action examples

```json
{"op":"subscribe","id":"sleep-state","streams":["power-sleep.changed"]}
{"op":"call","id":"lock","method":"powerSleep.lock","params":{}}
{"op":"call","id":"suspend","method":"powerSleep.suspend","params":{}}
{"op":"call","id":"hibernate","method":"powerSleep.hibernate","params":{}}
{"op":"call","id":"keep-awake","method":"powerSleep.setKeepAwake","params":{"enabled":true}}
```
