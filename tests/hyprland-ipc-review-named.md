# Named workspace reads

The bar publishes named regular workspaces in its workspace list, with `id: null`
and the real address. Active monitor/window numeric IDs are nullable too, with
companion address fields. Existing numbered JSON values stay numeric. Special
workspaces remain outside the ordinary list. The numbered strip and lifecycle
allocation exclude absent IDs; named contexts cannot bypass numbered guards.

The shared crate's captured 0.57.1 fixture tests the actual publication shape.
The handoff regression additionally checks a named v2 departure followed by a
return/close in one batch. Full `cargo test --locked` passes (364 passed, eight
ignored); the added named-v2 handoff case also passes after that run. Formatting
and diff whitespace checks pass. Shelllist's actual Qt strip tests validate a
null active ID does not select a numbered tile and pointer focus still routes
numeric IDs. Full named lifecycle commands remain a separate protocol extension.
