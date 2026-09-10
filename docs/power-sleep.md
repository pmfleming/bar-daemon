# Power and sleep

The `power_sleep` snapshot domain and `power-sleep.changed` stream are backed by systemd-logind. They expose the exact `CanSuspend` and `CanHibernate` capability values (`yes`, `no`, `challenge`, or `na`), the live `PrepareForSleep` state, and every current logind inhibitor with its owner, reason, mode, UID, and PID. The additive `diagnostics` object reports read-only kernel/swap evidence and ThinkPad guidance; it does not override logind's authorization or capability decisions.

The API provides `powerSleep.lock`, `powerSleep.suspend`, and `powerSleep.hibernate`. It resolves logind's current-session alias to a **concrete session object** and requests `Lock` only if not already confirmed locked. Lock completion has a five-second deadline. On Hyprland, a per-operation, read-only `hyprland-lock-notify-v1` connection supplies confirmation: its `locked` event follows the compositor's lock-screen transition barrier. Registry discovery and each subsequent protocol synchronization are bounded to two seconds. Already-locked sessions work, and unlock/disconnect events invalidate confirmation. No `hyprctl dispatch`, process-presence checks, input synthesis or manufactured `LockedHint` are used. In other desktop sessions, the existing logind `LockedHint` acknowledgement is required instead.

**Why the native Hyprland observer matters:** hypridle 0.1.7 receives compositor lock notifications but does not itself publish `SetLockedHint`. Waiting only for that hint can therefore fail on a correctly locked Hyprland/hyprlock session. The observer uses the actual completion event, not the earlier “lock requested” state. Hyprland detection uses `HYPRLAND_INSTANCE_SIGNATURE` or `XDG_CURRENT_DESKTOP`; the resident service must inherit the matching `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR`. A missing protocol or disconnected compositor fails closed rather than falling back to stale hints. Run one resident service for the primary graphical session.

Before sleep, the pinned session must still be active and locked, and logind must confirm it is not already preparing for sleep. These checks run again after any privileged setup; unreadable preparation/lock state is an error, not `false`. Failed locking never changes hibernate settings. Once logind accepts an action, a subsequent telemetry failure is returned as degraded status, **not** a failed operation inviting another sleep request. Suspend and hibernate remain non-interactive logind requests with no inhibitor bypass. A capability value of `challenge` remains selectable, but the actual request may be denied by non-interactive polkit; `no` and `na` are rejected before locking.

Hypridle remains the owner of idle detection and inhibitor handling. The optional managed integration below supplies its sleep timeout; bar-daemon does not add an independent inactivity timer.

## Automatic sleep, then hibernate

The `sleep_policy` snapshot domain and `sleep-policy.changed` stream expose the persisted policy, active profile (`shared`, `battery`, or `plugged`), integration availability, hibernation availability/reason, and last automatic-action failure. `powerSleep.setPolicy` accepts the complete policy:

```json
{"same_profile": false, "battery": {"sleep_minutes": 15, "hibernate_minutes": 60}, "plugged": {"sleep_minutes": 45, "hibernate_minutes": 180}}
```

Both fields are whole minutes, 0–10080. Zero means Never. `sleep_minutes` is inactivity before suspend; `hibernate_minutes` is **additional time asleep**, not time since last input. Never sleep disables automatic sleep; Never hibernate uses ordinary suspend. Shared mode uses the battery profile and preserves the plugged profile for later reuse. Initial settings are shared, 30 minutes to sleep, Never hibernate. Policies are atomically persisted under `$XDG_CONFIG_HOME/bar-daemon/sleep.json`. Invalid persisted data fails visibly instead of silently adopting defaults.

### Integration and ownership

Shelllist's Home Manager option `programs.shelllist.sleep.enable` replaces `hypridle.service`'s executable with:

```sh
bar-daemon idle --config "$HOME/.config/hypr/hypridle.conf" --hypridle /path/to/hypridle
```

The resident daemon gets `BAR_DAEMON_IDLE_CONFIG` pointing at that same base config. The wrapper preserves lock/DPMS listeners and logind/Wayland inhibitor settings, removes only simple two-setting `systemctl`/`loginctl` suspend/hibernate listeners, writes `$XDG_RUNTIME_DIR/bar-daemon/hypridle.conf`, then execs hypridle. Source includes, custom sleep command chains, and sleep listeners with additional resume/other settings are rejected: move non-sleep behavior into separate listeners before enabling integration. Undetectable custom scripts that sleep must also be removed manually. The declarative base file is never modified. If UPower or the saved policy cannot be read, the wrapper retains lock/DPMS listeners with automatic sleep disabled. If the base itself cannot be safely migrated, it runs the original configuration unchanged and the controls remain unavailable. Disabling the option restores the original hypridle service and policy.

Saving settings restarts hypridle; a failed restart restores the previous policy and attempts to restart it. AC changes select the matching profile and restart only when the inactivity delay changes, resetting the inactivity countdown (including lock/DPMS countdowns). The wrapper and idle callback both read UPower's current AC state rather than relying on stale UI telemetry. `powerSleep.idle` takes `sleep_minutes` and a per-start `generation` token from the generated listener. It refuses callbacks from a restarted countdown, a different timeout, or a Never profile. Sleep failures remain visible in `last_error` until a successful action or policy save. No action is automatically retried.

When hibernation is configured, the callback enters the same action guard as manual sleep, checks `CanSuspendThenHibernate`, confirms locking, and **only then** asks the privileged helper to set a bounded runtime-only hibernate delay. It re-checks active-session, lock and preparation state before invoking logind's `SuspendThenHibernate(false)`. There is no awake-process timer trying to count time while suspended. Systemd owns waking to hibernate, cancellation on normal wake, and protective early hibernation at critically low battery. Profile selection happens at sleep entry; changing AC power while asleep does not select a new delay for that cycle. Manual Suspend remains ordinary suspend and does not pick up this automatic policy.

The helper's polkit-gated `SetHibernateDelay(u32 minutes)` writes only `/run/systemd/sleep.conf.d/90-shelllist.conf`, containing `HibernateDelaySec` and `HibernateOnACPower=yes` so the plugged-in profile also works. It accepts no path, command or arbitrary config text; an idle callback cannot prompt for authentication. The runtime override is system-wide (as are systemd sleep settings), lasts until reboot or the next managed sleep, and affects other callers of suspend-then-hibernate too. Administrators can override it with a higher-priority sleep.conf drop-in. No swap provisioning, resume-device changes, bootloader changes or logind inhibitor bypasses are performed. Hibernation support and the updated system helper must be installed separately from a Home Manager-only UI update.

## Lenovo/ThinkPad diagnostics and remaining setup

Read-only checks (they never lock, suspend or hibernate):

```sh
bar-daemon debug sleep-diagnostics
bar-daemon debug lock-state
```

The reviewed host is a **Lenovo ThinkPad P14s Gen 5 AMD (21ME)**. Its kernel exposes `[s2idle]`, not `deep`; `amd_pmc` is loaded and bound to `AMDI0009:00`. Do not force `mem_sleep_default=deep`, write firmware/EC registers, unload drivers, or blanket-disable wake sources. For excessive sleep drain, first check Lenovo BIOS/EC updates and current kernel fixes, then measure AMD PMC/S0ix residency and inspect wake sources during a deliberately scheduled suspend/resume test. None of those hardware state changes or sleep cycles are performed by the diagnostics command.

On this host `/proc/swaps` lists **only `/dev/zram0`** and `/sys/power/resume` is `0:0`. Zram is volatile and cannot hold a hibernation image. Persistent, sufficiently sized disk-backed swap and working boot/initrd resume support are still required; merely enabling a timer cannot supply them. Swapfile setup is filesystem-specific (including Btrfs allocation rules and resume offsets), so no automatic swap allocation or bootloader changes are attempted. Conversely, `resume=0:0` alone does **not** prove hibernation impossible: modern systemd can discover persistent swap and record the target through EFI. Missing/unreadable evidence stays unknown. Kernel lockdown is reported as a possible policy restriction, never automatically disabled.

The per-profile delay continues to use systemd's suspend-then-hibernate and kernel wake timers, not a Lenovo-specific replacement or a daemon timer that stops during suspend. The helper's runtime delay is system-wide and remains subject to administrator drop-in precedence. End-to-end firmware wake/resume and hibernation reliability still need a controlled hardware test after persistent swap/resume is configured. The automated tests use isolated logind and Wayland protocol servers.

## Explicit action examples

```json
{"op":"subscribe","id":"sleep-state","streams":["power-sleep.changed"]}
{"op":"call","id":"lock","method":"powerSleep.lock","params":{}}
{"op":"call","id":"suspend","method":"powerSleep.suspend","params":{}}
{"op":"call","id":"hibernate","method":"powerSleep.hibernate","params":{}}
```
