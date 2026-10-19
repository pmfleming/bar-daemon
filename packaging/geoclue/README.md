# GeoClue location provenance extension

Owner: bar-daemon location/timezone integration. Based on GeoClue 2.7.2 (nixpkgs
source `mv1d6cgs3146dkx2hgsiv2dg44d457km-source`). The patch and test are
GPL-2.0-or-later, like the upstream files; they are applied to GeoClue, not linked
into the MIT Rust daemon.

`default.nix` appends `provenance.patch` and `freshness.patch` and enables their
Meson production-parser/cache test. `lib.mkLocationGeoclue`, `packages.locationGeoclue` and
`checks.locationGeoclue` expose the same implementation used by the NixOS module.
The module's ordinary GeoClue package override retains the patch when selecting
the demo-agent build variant.

## Contract

An additive read-only `Location.SourceKind` unsigned property is copied from
internal location state: 0 unknown, 1 IP, 2 non-IP. In the Mozilla-compatible
adapter, the request builder's internal `GeoIP` token means no cell/Wi-Fi data
was sent; `WiFi`/`3GPP` tokens describe non-IP inputs. Response `fallback: "ipf"`
is positive IP-fallback evidence; `lacf` is cell fallback. Other fallback values
are unknown. We do **not** parse the public human-readable Description or infer
source from accuracy. Both duplication paths retain provenance. Other adapters
remain unknown and require independent IP confirmation, just like non-IP sources.

Provider honesty/physical accuracy is not guaranteed. A provider that silently
falls back to IP without reporting it cannot be positively identified as IP;
it follows the confirmation branch. An unpatched GeoClue also follows that branch.
The daemon never silently upgrades missing/new provenance values to IP trust.

Wi-Fi cache hits retain observation timestamps instead of restamping old
observations. The authority rejects pre-network-epoch or expired evidence, not
all pre-attempt evidence. GeoClue's original whole-second precision remains.

The additive `Client.RefreshSince(t)` method lets an active, authorized owning
client request one revalidation, with a non-future Unix timestamp. Wi-Fi cache
selection excludes older entries before comparing accuracy; both Wi-Fi and IP
fallback share this path. Active web sources are refreshed even if another
client keeps them alive; an existing HTTP query is coalesced by upstream code.
There is no background revalidation timer, daemon restart, or timestamp rewrite.
Other clients can still use recent cache entries. Provider errors or missing
hardware remain pending evidence, bounded by the authority's acquisition deadline.

## Tests and upgrades

`provenance-test.c` invokes the production response parser for IP-only, Wi-Fi,
cell, explicit IP/cell fallback, null/unknown fallback and unknown request input.
It verifies source preservation through both copy paths and that mutating a
Description cannot change provenance. Malformed/missing/out-of-range coordinates
and invalid accuracy are rejected before GObject can default them to a fabricated
zero-valued location carrying IP provenance. Rust private-bus tests verify the consumer
wire contract, including unknown and absent properties, recent cache reuse and
one-shot revalidation with a source kept alive. The C test includes the production
Wi-Fi translation unit and exercises cache insertion/lookup, cutoff rejection,
selection of fresh over stale more-accurate entries, and IP-fallback caching
without constructing a live GeoClue source or contacting host services.

Build with `tools/local-build build --attr locationGeoclue ../bar-daemon` from the
framework checkout (or the equivalent source-launcher invocation in the policy
document). Patch application and the tests must pass on upgrades. Review request
input construction, provider fallback semantics, location copying, Wi-Fi cache
handling, D-Bus export and system-agent requirements before accepting a new
GeoClue version. Do not replace this with a Description substring heuristic.
