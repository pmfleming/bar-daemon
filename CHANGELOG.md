# Changelog

All notable changes to bar-daemon are documented here. The project follows Keep a Changelog conventions and will adopt semantic versioning for published releases.

## [Unreleased]

### Added

- Opt-in isolated Chromium web-app labels from the actual D-Bus owner's process
  environment, retaining per-instance MPRIS metadata, selection and controls.

- Shared event-driven compositor preference cache and `compositor.changed` stream,
  using native framework IPC with last-known-value retention and bounded retries.

- Native notification server, policy, history, persistence, actions, and D-Bus activation.
- Activity calendar, todo, and world-clock state.
- Local Rust quality measurement configuration.

### Changed

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
