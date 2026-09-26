# Display layouts

With `BAR_DAEMON_DISPLAY_CONTROL=1`, the daemon owns both docking policy and
saved layouts. Open Shelllist's dedicated Displays callout from the monitor bar
control, `SUPER+P`, or `shelllist open displays`. Its compact diagram expands into
a keyboard/mouse layout workspace with advertised modes, scale, position,
rotation/reflection and enablement of any output, including the laptop panel.
At least one display must remain enabled; another output must actually be usable
before each disable command. The UI blocks disabling the last enabled display.
Workspace rules remain ordinary declarative Hyprland configuration. This editor
supports extended and mirrored content, but does not provide bit-depth or
workspace-rule UI.

API additions (bar-api v1, existing `display-policy.changed` stream):

- `displayLayout.preview {outputs: [{name, mode, x, y, scale, transform, enabled, mirror_of}]}`
- `displayLayout.confirm {id}`
- `displayLayout.revert {id}`

Snapshot `display_policy.outputs` contains current compositor outputs and
advertised modes. Optional nonempty `description` supplies the compositor's human
readable monitor name; connectors remain request identity. `display_policy.layout` contains `saved` and an optional
`trial` with its opaque confirmation ID and `expires_at` Unix timestamp.
The additive `manual_enablement` flag defaults to false for older saved documents.
Confirming enable/disable or mirror/extend changes sets it: manual choices then override automatic
docking policy until `displayPolicy.set` is used again. Geometry-only edits do not
change this ownership. Previews preserve their requested enablement, and rollback
restores the actual previous enablement rather than forcing the laptop on.
If no usable output remains (for example after unplugging the only active external
monitor), the daemon still restores the laptop panel.
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

Enable independent replacements before attaching mirrors or disabling outputs,
and recheck that another independent output is usable immediately before each
disable. A mirror is not an independent fallback for its own source. Saved layouts are tried once
per stable topology/resume, after five seconds, rather than repeatedly resetting
working external modes. Unsupported saved modes surface an error and can be
replaced by a new preview. Automatic fallback restoration uses the saved internal
mode when possible and falls back to the compositor's preferred mode on failure.

## Mirror or extend

In **Settings → Display content**, choose **Extend desktop** or **Mirror <source>**
for each enabled screen, then Preview and Keep the whole layout. Mixed layouts
are supported: multiple mirrors can share one source while other screens extend.
Position controls are unavailable for mirrors; the arrangement canvas represents
one desktop tile per independent source and labels its copies. Returning to
Extend places the screen beside the other independent screens.

`mirror_of` is an optional connector string; missing/empty means extended, so old
saved documents and API clients keep their meaning. A mirror must reference a
connected, enabled, independent source in the proposal. Self-mirrors, missing or
disabled sources, chains and cycles are rejected before mutation. Each physical
screen retains its own advertised mode, scale and rotation. Hyprland scales the
source image to fit, with black bars when aspect ratios differ; identical
resolutions are not required. Position belongs to the source, not the mirror.

Native `mirrorOf` telemetry is normalized from numeric/string IDs and resolved to
connector names for drafts and recovery. Extended commands explicitly clear the
mirror rule. Confirmation checks the observed source relationship, not merely
command acceptance or overlapping geometry. Changing mirrors are first detached
so source handoffs do not create transient chains, then independent sources are
enabled before mirrors are attached. Automatic docking never disables a source
that still feeds active mirrors, and never counts a mirror as a standalone
replacement desktop.

Disabling a source in the UI promotes its copies to extended displays before the
source is disabled. During rollback or saved-layout recovery, an unplugged source
also causes surviving mirrors to become independent. Saved relationships are
retained for reconnect. Emergency laptop recovery always clears mirroring, so an
orphaned copy cannot masquerade as a usable desktop. Unconfirmed mirror/extend
changes use the same durable trial, expiry, sleep/restart and hotplug recovery as
mode/geometry changes. No live hardware switching is performed by the tests.

Keep the existing `monitors.lua` module, its Home Manager link and
`require("monitors")` intact as a startup/compatibility baseline. Existing
activated configurations may reference it through an out-of-store symlink:
removing the source file breaks the running configuration even without a rebuild.
The daemon does not rewrite that module.

No output changes, sleep or real docking are part of automated tests. Deploy the
daemon, UI and NixOS keybindings together before hardware verification.
