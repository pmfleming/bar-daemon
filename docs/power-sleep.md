# Power and sleep

The `power_sleep` snapshot domain and `power-sleep.changed` stream are backed by systemd-logind. They expose the exact `CanSuspend` and `CanHibernate` capability values (`yes`, `no`, `challenge`, or `na`), the live `PrepareForSleep` state, and every current logind inhibitor with its owner, reason, mode, UID, and PID.

The API provides `powerSleep.lock`, `powerSleep.suspend`, and `powerSleep.hibernate`. Suspend and hibernate first invoke `Lock` on logind's current-session object and then request the sleep action from logind without an interactive flag. A capability value of `challenge` is treated as available because an active session may authorize it through polkit; `no` and `na` are rejected before attempting sleep.

This design is safe for Hyprland because bar-daemon does not call compositor dispatchers, synthesize input, or replace the idle daemon. The session's lock implementation should listen for logind lock requests (for example, a `hyprlock` service integrated with the session). Existing `hypridle` configuration remains the owner of idle timeouts and automatic DPMS/suspend policy, avoiding two competing timers. Bar-daemon owns only status, explicit UI actions, and inhibitor visibility.

Examples:

```json
{"op":"subscribe","id":"sleep-state","streams":["power-sleep.changed"]}
{"op":"call","id":"lock","method":"powerSleep.lock","params":{}}
{"op":"call","id":"suspend","method":"powerSleep.suspend","params":{}}
{"op":"call","id":"hibernate","method":"powerSleep.hibernate","params":{}}
```
