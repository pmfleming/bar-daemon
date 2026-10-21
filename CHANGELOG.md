# Changelog

All notable changes to bar-daemon are documented here. The project follows Keep a Changelog conventions and will adopt semantic versioning for published releases.

## [Unreleased]

### Fixed

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
