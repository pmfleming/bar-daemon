# Monitor and window focus settings

Shelllist's **Displays → Focus** tab exposes the global Hyprland settings that
influence monitor activation, window focus, focus history and cursor warping.
It also shows the compositor-reported active monitor and the focused window's
monitor separately, using the existing workspace telemetry stream. Selecting a display in the
editor does **not** focus it or make these settings per-monitor.

The keyboard-focused window normally determines the active monitor. Pointer
monitor selection is a separate mechanism: disabling it alone does not prevent a
hovered window from receiving keyboard focus. For keyboard-led use, select
**Click to focus**, disable **Moving the pointer activates a monitor**, and review
floating-window and drag-and-drop exceptions. Cursor jumps and application
activation requests have independent controls.

There is no invented "lock focus to this monitor" switch. In particular, Hyprland's
explicit last-window and focus-monitor commands can cross monitors regardless of
`window_direction_monitor_fallback`. Keybindings, per-window rules, forced cursor
warps and application-specific behaviour are not rewritten by this page.

## Controls

Only values actually reported by the compositor are editable. Unsupported options
are labelled unavailable, not filled with defaults. These are the supported keys:

| Group | Keys |
| --- | --- |
| Mouse | `input:follow_mouse` (0–3), `misc:mouse_move_focuses_monitor`, `input:mouse_refocus`, `input:follow_mouse_threshold` (0–1000 logical pixels), `input:follow_mouse_shrink` (0–300 pixels), `input:float_switch_override_focus` (0–2) |
| Mouse exceptions | `misc:always_follow_on_dnd`, `misc:layers_hog_keyboard_focus`, `input:special_fallthrough` |
| Directional navigation | `binds:window_direction_monitor_fallback`, `binds:focus_preferred_method` (history/shared edge), `binds:movefocus_cycles_fullscreen`, `binds:movefocus_cycles_groupfirst` |
| Workspace history | `binds:workspace_back_and_forth`, `binds:allow_workspace_cycles`, `binds:hide_special_on_workspace_change` |
| Applications | `misc:focus_on_activate`, `input:focus_on_close` (0–2), `misc:on_focus_under_fullscreen` (0–2), `misc:initial_workspace_tracking` (0–2) |
| Cursor movement | `cursor:no_warps`, `cursor:persistent_warps`, `cursor:warp_on_change_workspace` (0–2), `cursor:warp_on_toggle_special` (0–2), `binds:workspace_center_on` (0–1), `cursor:warp_back_after_non_mouse_input` |

Unannotated keys are booleans. The UI describes each enum choice and relevant
interactions. Numeric controls require Enter or Apply; dropdown choices save
immediately and display the acknowledged value until the request completes.

## Protocol and persistence

The existing `display-policy.changed` stream and `display_policy` snapshot gain
`focus: {available, values, saved, error}`. `values` is observed state; `saved` is
only the subset explicitly overridden by the user. Monitor outputs additionally
expose `focused`. Focus telemetry does not invalidate a pending geometry draft.

- `displayFocus.set {"values": {"misc:mouse_move_focuses_monitor": false}}`
  patches one or more allowlisted values. It is not an arbitrary config/IPC API.
- `displayFocus.reset {}` restores the values captured before each setting's
  **first** edit and removes all Shelllist focus overrides. It does not guess
  factory defaults or reload the compositor configuration.

Overrides and their original values are persisted atomically in
`$XDG_CONFIG_HOME/bar-daemon/display-focus.json`. Opening the page without saved
overrides is read-only. Saved overrides are reconciled after daemon/compositor
restart, configuration reload and resume; matching settings are not rewritten.
The display-policy loop queries the allowlist in one IPC batch, normally every
two seconds. A changed setting is applied through fixed native `hl.config` Lua
and verified by reading compositor state before saving or acknowledging it.
Failed apply/verification/persistence attempts try to restore the previous live
values; rollback failure is reported rather than claimed as success.

Clearing overrides restores the previously captured values now; subsequent
Hyprland configuration reloads regain ownership. If the declarative config changed
while an override was active, the captured value is not that new config value.
Unsupported saved keys remain visible as an error and can be forgotten via Reset.

Changes require `BAR_DAEMON_DISPLAY_CONTROL=1`, an eligible active local Wayland
session, and no layout trial. The same display write lock serializes policy,
layout and focus mutations. Sleep preparation blocks writes; the sleep generation
is checked again immediately before mutation. The UI also blocks saves while a
layout draft is dirty or transport is unavailable. Focus failures are separate
from layout safety errors and do not themselves change output enablement.

No output mode, DPMS, lid, power policy or physical keybinding is modified.
Automated coverage uses fake compositor state and offscreen QML only; deployment
and deliberate hardware acceptance are separate steps.
