# Native city catalog (implementation stage 2)

`locations.json` beside the native todo store is a versioned, bounded, durable
saved-city catalog. Existing weather/world-clock config is imported once. Later
activity refreshes still read calendar configuration but do not restore deleted
cities. Home is protected Amsterdam; a previous non-Amsterdam Home is preserved
as an ordinary imported city. Legacy `auto` zones resolve offline from coordinates.

`locations.search` performs bounded HTTPS Open-Meteo geocoding reads (12 candidates,
64 KiB, 10-second deadline, serialized/rate-limited). `locations.add` and
`locations.remove` require the observed catalog revision. Explicit stale requests
fail; writes are never retried automatically. Search does not persist anything.
Exact duplicate places resolve to existing IDs; same-zone distinct cities remain.
Corrupt/unsupported stores fail closed with read-only protected Amsterdam still
projected; config is not rewritten. A previously imported catalog remains usable
if subsequent calendar configuration is malformed. The store allows at most 32
cities including Home (plus the derived Current row); this also bounds duplicated
forecast payloads in Activity snapshots. Imports exceeding the bound are rejected,
not silently truncated.

The catalog projects clocks immediately, independent of weather availability.
Refresh and catalog mutations serialize so late forecasts cannot resurrect removed
cities. Snapshot publication has its own serialization guard so a previously
computed projection cannot overwrite a newer catalog publication. Unrelated
forecast caches survive changes. Each forecast fetch has a 15-second deadline
so mutations do not wait indefinitely behind provider I/O. Home and Current are protected;
Current is suppressed when the accepted location resolves to Home's canonical
zone. Home then gains the Current role without replacing its fixed coordinates.
Unknown accepted location remains an unavailable separate Current row.

Final validation: `cargo test --locked --all-targets` passed 156 library tests,
34 authority tests and one startup integration test (three existing environment-
dependent tests ignored). Strict Clippy passed for the library and authority binary.
The protocol-fixture freshness check passes. New regression coverage exercises real
add/remove API replies, revision/protected-role rejection, malformed configuration
on restart and corrupt catalog fallback. Service unit fixtures block live forecast
HTTP. No host service, timezone or provider was changed. Authority controls are
implemented separately; see `time-location-controls.md`.
