# Generic result ranking

The `shelllist-search` framework library owns the fuzzy/typo matching algorithm.
Bar serves ephemeral result catalogs through `search.catalog`, `search.query`
and `search.release` on both ordinary v2 transports. This replaces the frontend's
separate JSONL search worker; provider-ranked domain queries keep their own order.

Catalog names are scoped to the physical caller. Revisions increase, identical
retransmissions are idempotent, conflicting/stale catalogs reject, and queries
must target the current revision. Connection loss clears every caller catalog.
Release removes a single catalog without affecting another caller with the same
name. No catalog/search text is persisted or published on event streams.

Per client: 128 catalogs and a 4 MiB logical catalog budget (including item and
keyword storage overhead). Each catalog has at most 20,000 items. At most 64
clients and two blocking ranking jobs are admitted. Native service admission
bounds encoded requests and replies separately. Busy ranking may be retried as a
read; malformed or over-capacity catalogs must be corrected rather than replayed.

The private socket tests in `src/daemon/search/tests.rs` cover ranking, fractional
scores, Unicode, catalog revisions/retransmissions, caller isolation, release,
bounds and cleanup. The existing production socket/D-Bus parity fixture exercises
all three methods without starting monitors or touching host services.
