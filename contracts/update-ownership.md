# Update API delegation

update-daemon is the owner of update-specific behavior and wire types. The staged
migration moves the current updates module and its behavioral tests to that
project, then replaces it with transport forwarding to its session service.
Generic desktop notification delivery and transport remain reusable services.
No final bar-daemon implementation reads update JSON or Turso files, interprets
update phases, merges rebuild history or decides which update incidents notify.

Existing method names, stale-cursor errors and updates.changed remain compatible.
See ../../update-daemon/contracts/ownership.md for the privilege/storage contract.
