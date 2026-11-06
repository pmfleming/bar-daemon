# Changelog

All notable changes to bar-daemon are documented here. The project follows Keep a Changelog conventions and will adopt semantic versioning for published releases.

## [Unreleased]

### Changed

- Give agenda, locations, weather, clocks and timezone their own activity process.
  Each session daemon exposes only its owned methods, snapshots and streams.

- Move workspaces, display policy, work area and compositor preferences to
  `bar-compositor-daemon`. Compositor session loss stops only this process.

- Isolate battery, power profiles and sleep policies in `bar-power-daemon`;
  other consumers observe logind sleep transitions independently.

- Run notifications in their own session process, with independent startup,
  compositor routing, persistence, activation and Shelllist subscriptions.

- Extract the privileged battery helper and location authority into independent
  workspace crates. Share only battery helper contracts and validated place data.

### Fixed

- Generate standalone activation and units from the same service specifications
  used by NixOS and Home Manager; remove the residual bar notification setting.

- Share preparation, initial-read and suspend-clock resume semantics in the
  daemon framework across policy owners and read-only observers.

- Scope component default logging to the daemon and its framework instead of
  enabling informational logs from every dependency; preserve RUST_LOG overrides.

- Generate Shelllist routing from the explicit native ownership registry and
  reject unregistered streams; route application workspace requests to compositor.

- Hold the legacy monolith bus name for the entire agenda lifetime. The status
  endpoint now uses `org.laufan.BarStatusDaemon`, so legacy startup cannot race
  an active activity store and activity does not depend on status responsiveness.

- Prepare search catalogs on bounded blocking workers outside the store lock;
  release and disconnect fence in-flight preparations against resurrection.

- Align direct D-Bus, socket, stdio bridge and Shelllist buffering bounds with
  the documented search catalog allowance, including JSON escaping overhead.

- Reserve startup privately and claim public D-Bus names only after persistent
  stores and interfaces are ready, including the freedesktop notification name.

- Exit safe session owners after a monitor panic or unexpected return so their
  service restart policy can recover them, retaining conservative power policy.

- Retry failed battery alert delivery after a bounded backoff while the condition
  remains current, and consume alerts only after successful acknowledgement.

- Reconnect native notification routing to verified replacement compositors,
  coalesce monitor events, and skip routing for external notification backends.

- Continue the critical-battery grace period when the notification server is
  unavailable, retaining the warning error and all sleep safety checks.

- Give an empty temporary workspace the icon of the app moved into it. Replace
  the icon when a different app fills it later, retain it while occupied, and
  preserve standard workspace category icons. Reuse the desktop icon cache.

- Route numeric workspace focus/move commands through the lifecycle owner. Reject
  missing extra destinations without creating them, while preserving standard
  destinations and retained manual workspaces; moves still follow the window.

- Keep newly empty launch-created workspaces for a two-second grace period.
  Preserve their destination across verified same-process login/main-window
  replacements without following focus. Explicit close, manual moves, process
  identity changes and owner restart cancel replacement tracking.

- Keep manually created workspaces until an explicit workspace close, including
  after switching away, emptying them, or restarting the daemon. Preserve their
  destination when Hyprland reclaims an empty inactive compositor workspace.

- Explicitly release agenda database ownership on shutdown and failed startup,
  so descriptors temporarily inherited by subprocesses cannot block reopening.

- Use GeoClue first for automatic timezone updates. Accept positively identified
  IP-only/fallback results without another IP request; confirm all other sources
  by canonical coordinate-derived timezone equality, without distance, country
  or Wi-Fi-only accuracy restrictions. Missing confirmation retries; actual
  timezone disagreement leaves the system unchanged. This supersedes the recent
  direct NetworkManager/beaconDB acquisition and connection-scoped 404 stop.
  Package a tested GeoClue SourceKind extension instead of trusting Description;
  preserve cached observation age. Restore GeoClue service-account authorization,
  remove direct scan privilege, retain write-safety fences and legacy cooldowns,
  and prevent independent IP cooldowns from blocking GeoClue-IP acceptance.

- Recover notification-center read contention natively: wait for reader capacity
  within one three-second deadline and retry only SQLite BUSY/LOCKED reads up to
  three attempts. Exhausted/permanent failures remain explicit; cancellation,
  failed-write fences, revisions and all mutation no-replay guards are preserved.
- Require a valid `HYPRLAND_INSTANCE_SIGNATURE` and a reachable socket for that
  exact instance before changing monitor layouts, output enablement, workspace
  focus, or compositor focus settings. Stale sessions cannot redirect mutations
  to another compositor; read-only discovery retains fallback.
- Report Hyprland event connection/read failures, including startup failures,
  and invalidate compositor caches while retaining recovery polling. Repeated
  identical connection failures are deduplicated by the shared transport.

- Preserve bounded image-path and desktop-entry hints in notification-center
  previews, so app lists and compact headers can resolve sender artwork for
  both live and archived notifications instead of falling back to a bell.

### Added

- Update-history lane/outcome filters (including Problems), local-calendar date
  search, bounded retention anchors for atomic multi-page refresh and a typed
  stale-cursor response. Filters/counts cover the retained catalog; recovery
  annotations still include evidence outside the selected filter.

- Read-only `updates.getOverview` for worker-authoritative deployment, baseline,
  installed tool/check times, schedules and bounded unresolved issues. Annotate
  historical recovery without changing outcomes; expose manual failed-stage and
  deployment fields and include outcome codes in history search. Cover SNI
  registration when the tray watcher starts after the Updates item.

- A stable **Current location** entry backed by the last accepted GeoClue estimate
  (including approximate IP positions), with source/accuracy metadata,
  read through an active-local-session-authorized interface without modifying
  saved locations. Rejected fixes and temporary reader outages retain it.
- Location-attempt diagnostics for provider acquisition, evidence comparisons,
  precise disagreement reasons, retry scheduling and verified timezone effects.
  Coordinates and network identifiers stay out of authority logs.

- Optional notification period view: three newest records followed by disjoint
  Today / This week / This month / Older groups. Previews never duplicate group
  records; calendar-day fencing prevents mixed windows across local midnight.
- Date-grouped notification timelines with bounded exact-repeat stacks and
  revision-fenced scrolling windows; no frontend grouping of partial history.
- Persistent per-app silence, timed quiet periods, grouping and DND bypass.
  Policy acknowledgements follow storage commits.
- Snapshot-confirmed deletion across retained and active notifications, preserving
  later arrivals and application settings. Deletion is distinct from legacy
  dismissal and includes history older than the recent search window.
- Desktop-file application icon capture, persisted separately from notification
  content imagery and exposed in bounded center previews.

- `notifications.queryCenter`: native app groups and counts, bounded previews,
  five-message pages with direct seeking, selected-record lookup and
  epoch/revision-fenced app paging. Full-scope search and lifecycle/action guards
  remain daemon-owned; clients no longer need to download/group message bodies.

- Optional YouTube oEmbed fallback for missing title/channel/artwork, enabled by
  `BAR_DAEMON_YOUTUBE_METADATA=1`. Bounded fixed-endpoint requests, session caching,
  private temporary thumbnails and owner/content-generation checks leave MPRIS
  controls and supplied metadata authoritative. No cookies or browser extensions.
  Filled fields carry additive `metadata_sources` provenance.

- Opt-in isolated Chromium web-app labels from the actual D-Bus owner's process
  environment, retaining per-instance MPRIS metadata, selection and controls.

- Shared event-driven compositor preference cache and `compositor.changed` stream,
  using native framework IPC with last-known-value retention and bounded retries.

- Native notification server, policy, history, persistence, actions, and D-Bus activation.
- Activity calendar, todo, and world-clock state.
- Local Rust quality measurement configuration.

### Changed

- Replace notification/preview JSON blobs and JSON preferences with typed Turso
  v2 rows and ordered action tables. Start fresh in `notifications-turso-v2.db`;
  SQLite/v1 history, DND, settings, policies, ID counters and once-only keys are
  not imported. Legacy files remain untouched; exact-path overrides must select
  a fresh file. Retain transactional cleanup, crash recovery and bounded history
  semantics. Pair with Shelllist's reconnect reset so old pages/actions/deletion
  tokens cannot survive into the new session. See `docs/notifications-storage.md`.

- Keep name-only notification senders in one app group when their notification
  icons change; preserve separate groups for distinct desktop-entry identities.

- Recover isolated Pocket Casts and Audible PWA labels when Chromium drops launcher
  environment hints, requiring matching app URL, class and per-app profile from
  the actual D-Bus owner; playback IDs and metadata remain unchanged.

- Ignore update-monitor read events and coalesce write bursts to prevent idle
  refresh loops; recover watches after directory replacement or watcher failure.
- Require `ready-created-at` before reporting delayed updates ready, matching the
  privileged worker's completeness check.

- Normalize display mode catalogs, exact current modes, connector capabilities and
  mirror sources in Rust for all bar-api display snapshots and events.

- Split API, daemon orchestration, and battery integration into focused modules.
- Removed avoidable production panic paths and expanded policy tests.

## [0.1.0]

- Initial bar state, control API, and system integration release.
