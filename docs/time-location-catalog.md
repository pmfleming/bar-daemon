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
Corrupt/unsupported stores fail closed; config is not rewritten.

The catalog projects clocks immediately, independent of weather availability.
Refresh and catalog mutations serialize so late forecasts cannot resurrect removed
cities. Unrelated forecast caches survive changes. Home and Current are protected;
Current is suppressed when the accepted location resolves to Home's canonical
zone. Home then gains the Current role without replacing its fixed coordinates.
Unknown accepted location remains an unavailable separate Current row.

Validation: locked library run: 153 passed, one existing ignored test; the sole
failure was the expected protocol-fixture freshness assertion. Regenerated the
native fixture and reran its check. Service unit fixtures block live forecast HTTP.
No host service, timezone or provider was changed. Authority controls and UI are
subsequent stages; these APIs do not yet expose manual/automatic control.
