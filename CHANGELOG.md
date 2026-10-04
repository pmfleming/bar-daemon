# Changelog

All notable changes to bar-daemon are documented here. The project follows Keep a Changelog conventions and will adopt semantic versioning for published releases.

## [Unreleased]

### Added

- Shared event-driven compositor preference cache and `compositor.changed` stream,
  using native framework IPC with last-known-value retention and bounded retries.

- Native notification server, policy, history, persistence, actions, and D-Bus activation.
- Activity calendar, todo, and world-clock state.
- Local Rust quality measurement configuration.

### Changed

- Normalize display mode catalogs, exact current modes, connector capabilities and
  mirror sources in Rust for all bar-api display snapshots and events.

- Split API, daemon orchestration, and battery integration into focused modules.
- Removed avoidable production panic paths and expanded policy tests.

## [0.1.0]

- Initial bar state, control API, and system integration release.
