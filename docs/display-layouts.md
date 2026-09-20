# Display layouts

With `BAR_DAEMON_DISPLAY_CONTROL=1`, the daemon owns both docking policy and
saved layouts. Open Shelllist's dedicated Displays callout from the monitor bar
control, `SUPER+P`, or `shelllist open displays`. Its compact diagram expands into
a keyboard/mouse layout workspace with advertised modes, scale, position,
rotation/reflection and external-output enablement. Laptop enablement remains
owned by the external-only preference; previews always retain its fallback.
Workspace rules remain ordinary declarative Hyprland configuration. This editor
does not provide nwg-displays' mirroring, bit-depth or workspace-rule UI.

API additions (bar-api v1, existing `display-policy.changed` stream):

- `displayLayout.preview {outputs: [{name, mode, x, y, scale, transform, enabled}]}`
- `displayLayout.confirm {id}`
- `displayLayout.revert {id}`

Snapshot `display_policy.outputs` contains current compositor outputs and
advertised modes. Optional nonempty `description` supplies the compositor's human
readable monitor name; connectors remain request identity. `display_policy.layout` contains `saved` and an optional
`trial` with its opaque confirmation ID and `expires_at` Unix timestamp.
Requests must cover all supported connected outputs. Numeric bounds, connector
names, advertised modes and final active/local-session checks are enforced by
the daemon, not trusted to QML. Compositor commands are fixed native Lua calls;
user-provided executable strings are never accepted.

Before any preview mutation, the previous layout and proposal are atomically
journalled to `$XDG_CONFIG_HOME/bar-daemon/display-layout.json`. Only explicit
confirmation of the observed layout and unchanged connector/ID topology commits it.
A topology change also triggers early rollback on the next reconciliation. Docking
policy writes are rejected while the durable journal contains a trial, including
after daemon restart; independent clients cannot change policy mid-preview. Unconfirmed changes revert
after 20 seconds (with the two-second reconciliation interval), on daemon
restart or on resume. A monotonic deadline also prevents a backwards wall clock
from extending a live preview. Sleep preparation defers all mutations until
resume. Lost clients cannot leave a permanently unconfirmed layout. Failed
rollback retains the journal for retry; automatic policy keeps the internal
fallback rather than disabling it on a layout error.

Enable replacements before disabling external outputs, and recheck that another
output is usable immediately before each disable. Saved layouts are tried once
per stable topology/resume, after five seconds, rather than repeatedly resetting
working external modes. Unsupported saved modes surface an error and can be
replaced by a new preview. Automatic fallback restoration uses the saved internal
mode when possible and falls back to the compositor's preferred mode on failure.

Keep the existing `monitors.lua` module, its Home Manager link and
`require("monitors")` intact as a startup/compatibility baseline. Existing
activated configurations may reference it through an out-of-store symlink:
removing the source file breaks the running configuration even without a rebuild.
The daemon does not rewrite that module.

No output changes, sleep or real docking are part of automated tests. Deploy the
daemon, UI and NixOS keybindings together before hardware verification.
