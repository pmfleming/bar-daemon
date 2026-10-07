# Changelog

All notable changes to bar-daemon are documented here. The project follows Keep a Changelog conventions and will adopt semantic versioning for published releases.

## [Unreleased]

### Fixed

- Preserve bounded image-path and desktop-entry hints in notification-center
  previews, so app lists and compact headers can resolve sender artwork for
  both live and archived notifications instead of falling back to a bell.

### Added

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
