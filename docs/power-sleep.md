# Power and sleep

The `power_sleep` snapshot domain and `power-sleep.changed` stream are backed by systemd-logind. They expose the exact `CanSuspend` and `CanHibernate` capability values (`yes`, `no`, `challenge`, or `na`), the live `PrepareForSleep` state, and every current logind inhibitor with its owner, reason, mode, UID, and PID.

The API provides `powerSleep.lock`, `powerSleep.suspend`, and `powerSleep.hibernate`. All three actions invoke `Lock` on logind's current-session object and wait up to five seconds for its `LockedHint` to become true. The `Lock` method reply alone is not confirmation. Suspend and hibernate request sleep without an interactive flag only after this acknowledgement. A lock error, unavailable hint, or timeout fails the action without requesting sleep. A capability value of `challenge` is treated as available because an active session may authorize it through polkit; `no` and `na` are rejected before attempting sleep.

Bar-daemon does not call compositor dispatchers, synthesize input, or replace the idle daemon. The session's lock implementation must listen for logind lock requests (for example, a `hyprlock` service integrated with the session) and call logind's `SetLockedHint(true)` only after the compositor acknowledges that the session is locked. It must clear the hint on unlock. Merely launching a locker process is not sufficient. Integrations that do not maintain this acknowledgement fail closed: explicit sleep requests time out instead of sleeping unlocked. Hypridle remains the owner of idle detection and inhibitor handling. The optional managed integration below supplies its sleep timeout; bar-daemon does not add an independent inactivity timer.

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

When hibernation is configured, the callback checks `CanSuspendThenHibernate`, asks the privileged helper to set a bounded runtime-only hibernate delay, and invokes logind's `SuspendThenHibernate(false)` **only after confirmed locking**, with the same fail-closed rules as explicit actions. There is no awake-process timer trying to count time while suspended. Systemd owns waking to hibernate, cancellation on normal wake, and protective early hibernation at critically low battery. Profile selection happens at sleep entry; changing AC power while asleep does not select a new delay for that cycle. Manual Suspend remains ordinary suspend and does not pick up this automatic policy.

The helper's polkit-gated `SetHibernateDelay(u32 minutes)` writes only `/run/systemd/sleep.conf.d/90-shelllist.conf`, containing `HibernateDelaySec` and `HibernateOnACPower=yes` so the plugged-in profile also works. It accepts no path, command or arbitrary config text; an idle callback cannot prompt for authentication. The runtime override is system-wide (as are systemd sleep settings), lasts until reboot or the next managed sleep, and affects other callers of suspend-then-hibernate too. Administrators can override it with a higher-priority sleep.conf drop-in. No swap provisioning, resume-device changes, bootloader changes or logind inhibitor bypasses are performed. Hibernation support and the updated system helper must be installed separately from a Home Manager-only UI update.

## Explicit action examples

```json
{"op":"subscribe","id":"sleep-state","streams":["power-sleep.changed"]}
{"op":"call","id":"lock","method":"powerSleep.lock","params":{}}
{"op":"call","id":"suspend","method":"powerSleep.suspend","params":{}}
{"op":"call","id":"hibernate","method":"powerSleep.hibernate","params":{}}
```
