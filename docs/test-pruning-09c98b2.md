# Test reduction from `09c98b2`

## Result

| Registered tests | Before | After |
|---|---:|---:|
| Rust unit tests | 219 | 144 |
| Rust integration tests | 2 | 2 |
| Python tooling tests | 7 | 7 |
| **Total** | **228** | **153** |

**75 fewer tests (32.89%); 67.11% retained.** The baseline includes the
private-service tests and the crash-child harness registration. Historical pruning
reports describe earlier passes, not this baseline.

This pass removes tests and abandoned test helpers/imports/modules, not production
behavior. No tests were ignored, disabled, moved out of discovery, or replaced with
one catch-all runner. Seven closely related checks were retained inside existing
lifecycles: todo save failure after load recovery, charge-once restoration, protected
charge-complete alerts, weather overflow, future notification snoozes, inline reply
validation, and confirmed single-record deletion/cancellation. Other removals are
real reductions in test coverage, not merely fewer registrations for the same suite.

Rust source shrank by **2,809 lines net** (108 added, 2,917 removed), excluding this
document. All seven Python tooling safeguards and both Rust integration tests remain.

## Validation and trade-offs

Both before and after used `tools/quality-fixtures.sh coverage`, with fresh isolated
PipeWire, D-Bus and hypridle fixtures and `--include-ignored`. The final run passed
144 unit + 2 integration tests with **zero ignored**. Python discovery passed 7/7.
Strict all-target Clippy, package formatting and `git diff --check` passed.

| LLVM Cobertura reported line coverage | Before | After |
|---|---:|---:|
| Covered / executable lines | 16,757 / 21,683 | 14,389 / 20,417 |
| Line coverage | 77.28% | 70.48% |

Coverage fell **6.81 percentage points** (unrounded calculation). These are the raw
workspace report totals, including instrumented test/support code; the denominator
changes when tests are removed. This is **not** a production-only comparison or a
claim that equivalent regression detection was preserved. Branch coverage was not
collected. No mutation, audit, or full strict-policy certification is claimed.

Instrumented unit test execution was 9.93s before and 9.57s after in one run each;
that is not sufficient evidence of a reliable speedup. The principal benefit is
less test code and fewer implementation-specific assertions to maintain.

Local reproducible evidence is under `target/test-pruning-current/`: baseline and
final inventories, `baseline.tar`, exact removed bodies in `removals.json`, before/
after Cobertura reports and fixture logs, `clippy.log`, and `python.log`. Reproduce:

```sh
cargo test --workspace --all-targets --locked -- --list
python3 -B -m unittest discover -s tools -p 'test_quality_*.py' -v
QUALITY_OUTPUT_DIR=<fresh-directory> bash tools/quality-fixtures.sh coverage
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --package bar-daemon -- --check
git diff --check
```

The fixture runner requires its documented Nix/private-service environment; it
must not be substituted with tests against host services.

## Retirement audit

All 75 retired registrations are listed below by original fully qualified name.
Retained tests are behavioral anchors, **not automatic substitutes for every
retired assertion**. Unless a check is explicitly described as preserved in an
existing lifecycle, its dedicated scenario coverage is intentionally reduced.
Priority was given to self-checking fixtures, passive projection/compatibility
matrices, internal timing/cache counts, and overlap with live boundaries. Essential
side-effect safety, input trust boundaries, durable write failures and actual
service contracts remain the focus of the retained suite.

### Activity projection and provider details (6)

Retained `membership_handles_dst_exclusive_ends_and_undated_todos`, `date_only_queries_use_local_half_open_days_including_dst_and_partial_days`, `zero_duration_events_belong_to_their_day_including_range_start`, calendar parser/source-recovery tests, and polar/unavailable astronomy checks. Retired duplicate interval-helper checks, example-config self-checks, and exact passive location/lunar projections. Dedicated duplicate-source validation, lunar reference values, weather cache buffer identity/freshness/fallback, and location deduplication coverage is reduced. Weather normalization retains representative overflow rejection, not the full malformed-payload/time-field matrix.

- `activity::astronomy::tests::reference_quarters_and_pre_reference_dates_are_normalized`
- `activity::config::tests::documented_config_is_valid_but_duplicate_source_ids_are_rejected`
- `activity::day::tests::query_interval_distinguishes_instants_and_exclusive_ends`
- `activity::locations::tests::locations_have_stable_identities_and_weather_supersedes_duplicate_clocks`
- `activity::weather::tests::cache_hits_and_failures_reuse_forecast_buffers_without_polling_fresh_fetches`
- `activity::weather::tests::malformed_payloads_and_overflowing_provider_times_are_errors`

### Notification projections and optional presentation (7)

Retained full-scope grouping/seeking, live/archive deduplication, transient replacement, catalog search/restart fences, pagination/cursor rejection, timeline stack bounds, and midnight continuation invalidation. Retired detailed artwork/group identity projections, desktop-icon refresh mechanics, SwayNC compatibility parsing, precise clipping/period boundary matrices, and the deleted refresh-anchor scenario. Those details lose dedicated coverage; general pagination is not an equivalent deleted-anchor regression test.

- `activity::notifications::center_tests::center_ignores_notification_artwork_when_grouping_named_senders`
- `activity::notifications::center_tests::center_keeps_artwork_hints_in_live_archived_and_restarted_previews`
- `activity::notifications::center_tests::center_validates_queries_and_does_not_merge_unnamed_senders`
- `activity::notifications::history_tests::refresh_anchor_completes_even_when_the_oldest_visible_record_was_deleted`
- `activity::notifications::identity::tests::refresh_discovers_installs_updates_and_removals_without_restart`
- `activity::notifications::swaync::tests::parses_waybar_status_and_dnd_class`
- `activity::notifications::timeline::tests::periods_use_local_calendar_boundaries_with_week_precedence`

### Notification engine scheduling and replies (2)

Retained `expires_and_emits_close_reason`, popup-expiry actionability, and the real D-Bus `client_receives_actions_tokens_replies_and_closure`. The timer lifecycle now includes a future snooze, and the D-Bus test rejects unsupported and blank inline replies. Removed internal deadline-consumption accounting and the separate timeout-policy/inline-capability test; the full urgency/transience timeout matrix is deliberately not retained.

- `activity::notifications::engine::tests::expiry_consumes_due_deadlines_once_without_waking_future_snoozes`
- `activity::notifications::engine::tests::timeout_policy_and_inline_reply_capabilities`

### Notification storage and deletion (5)

Retained real SQLite contention/recovery, cancelled reservations/accepted writes, failed-write fences, retention/restart IDs, and confirmed deletion across active/saved rows. The existing confirmed-delete lifecycle now checks wrong record identity, cancellation, one-record deletion, and untouched other records. Removed duplicate queue setup/progression, exact retry-count/permanent-error matrices, and the 5,005-row deletion scenario. Dedicated maximum search-window deletion and permanent/poisoned read-retry assertions are reduced; ordinary deletion tests do not establish the large-history boundary.

- `activity::notifications::persistence::tests::center_recovery_is_bounded_and_never_retries_permanent_or_poisoned_reads`
- `activity::notifications::persistence::tests::live_and_saved_repeat_keys_agree_at_unicode_clipping_boundaries`
- `activity::notifications::persistence::tests::saturated_queue_preserves_ordered_mutations_and_restart_state`
- `activity::notifications::redesign_tests::cancelled_confirmation_and_single_record_identity_are_safe`
- `activity::notifications::redesign_tests::delete_includes_retained_records_older_than_the_search_window`

### Todo persistence (1)

Retained the live API todo lifecycle and extended the existing failed-load/recovery test, renamed `failed_todo_storage_blocks_mutations_and_recovers_without_data_loss`, to obstruct the next save. Create/complete/delete must all fail without changing either memory or the published snapshot; successful durable recovery is checked. This removes repeated service setup rather than weakening save-before-publish checks.

- `activity::service::tests::failed_todo_save_never_publishes_a_candidate`

### API-specific small checks (3)

Retained snapshot validation, live todo mutation/persistence, live notification mutations/queries, and battery decoder rejection with no effect dispatch. Removed the separate refresh-ack timing threshold, generic routing rejection sample, and battery device-selection helper matrix. Dedicated async refresh latency, disabled-notification-backend routing, and default/missing/unsupported battery selection coverage is reduced.

- `api::activity::tests::refresh_acknowledges_before_isolated_provider_completion`
- `api::battery::tests::device_selection_preserves_default_missing_and_unsupported_errors`
- `api::tests::routing_preserves_backend_and_validation_errors`

### Audio queue implementation (1)

Retained real private-PipeWire controls/external changes, hardware-route and POD decoding tests, and failed-readback state/error-contract protection. Removed the private batching algorithm test: exact coalescing counts, clamp/direction boundaries, and mute ordering inside a batch lose direct assertions. The real backend fixture is broader behavioral evidence, not proof of every batching edge case.

- `audio::controller::tests::coalesces_queued_repeats_without_reordering_mutes_or_direction_changes`

### Battery helper duplicates and passive history (5)

Retained isolated helper threshold validation/readback, unknown-authorization fencing, restoration-marker verification, calibration safety, battery forecast bounds, energy integration, and sleep/unobserved-time exclusion. The uncertain-write lifecycle now restores 60/85 rather than configured 75/80 and also checks active charge-once targeting; the alert lifecycle now checks the protected charge-complete threshold. Retired the duplicate ID/threshold helper matrix and passive history-restart fixture. Exact history restart continuity and the readiness-monitor idle/drop scenario lose dedicated tests.

- `battery::config::restoration_tests::battery_identifiers_and_threshold_boundaries_are_validated`
- `battery::history::tests::restart_preserves_samples_without_counting_downtime`
- `battery::monitor::tests::readiness_monitor_survives_idle_periods_and_stops_on_drop`
- `battery::policy::tests::protected_limit_counts_as_charge_complete`
- `battery::tests::charge_once_restores_the_preexisting_thresholds`

### Brightness discovery and reply projection (2)

Retained panel-safe bounds, concurrent publication, fallback preservation/gate release, and timed-out helper termination/recovery. Removed the extra quantized reply/fixture projection and discovery-with-partially-bad-root cases. Their exact serialized reply and partial-discovery combinations lose dedicated assertions.

- `brightness::tests::control_reply_uses_real_quantized_readback_and_rejection_keeps_committed_state`
- `brightness::tests::discovery_skips_bad_devices_but_reports_a_completely_unusable_root`

### D-Bus cache implementation (2)

Retained real peer RPC timeout/no-replay/concurrent-progress behavior. Removed exact connection-cache acquisition/reconnect counts and cancelled setup/cache recovery. Reconnect/setup cancellation no longer has a dedicated cache-level test; the retained RPC timeout test does not prove those cases.

- `bus::tests::cancelled_or_timed_out_setup_does_not_poison_the_cache`
- `bus::tests::concurrent_acquisitions_reuse_and_disconnect_reconnects_once`

### Compositor and workspace cache mechanics (3)

Retained reload-during-read/reconnect invalidation and slow workspace reads with live invalidations/disconnection recovery. Removed the second cache retry/dedup test, exact workspace retry timer/count test, and configured-instance mutation matrix. Dedicated workspace mutation selection/no-fallback checks and failure-without-new-events retry timing are reduced.

- `compositor::tests::cache_retries_errors_preserves_last_known_and_deduplicates_events`
- `hyprland::tests::workspace_failures_retry_without_new_events_but_success_stops_polling`
- `hyprland::tests::workspace_mutations_require_a_reachable_configured_instance`

### Display projection and overlapping lifecycle tests (11)

Retained closed-schema/injection rejection, verified/persistent focus changes and rollback; layout token/readback verification; last-display protection; replacement/source usability rechecks; restart/resume/expiry recovery; stale preview and replaced connector rejection; mirror fallback/source handoffs; observed-enablement reconciliation; and final hotplug/sleep safety checks. Removed compatibility parsing, fixture self-comparison, private transition timing, and overlapping docking/manual fallback scenarios. Dedicated external-only saved-layout fallback, pending-docking preview exclusion, manual-policy tick interaction, intermediate unplug readback state, and stability/sleep-budget timing coverage is reduced; retained safety tests are not claimed to cover every removed interleaving.

- `display_policy::focus::tests::reads_observed_values_and_omits_unsupported_options_without_defaults`
- `display_policy::layout::outputs::tests::disabled_and_missing_modes_do_not_invent_geometry`
- `display_policy::layout::outputs::tests::mirror_references_are_resolved_against_one_snapshot`
- `display_policy::layout::tests::external_only_saved_layout_defers_to_fallback_when_undocked`
- `display_policy::layout::tests::preview_cannot_race_an_acknowledged_but_unobserved_docking_change`
- `display_policy::layout::tests::real_preview_confirmation_and_rollback_match_control_fixtures`
- `display_policy::tests::manual_enablement_survives_policy_ticks_but_unplug_restores_fallback`
- `display_policy::tests::output_flaps_mode_changes_and_observation_gaps_restart_stability`
- `display_policy::tests::unplug_during_readback_keeps_applying_until_the_fallback_is_observed`
- `display_policy::transition::tests::acknowledgement_waits_for_observation_and_timeout_has_explicit_recovery`
- `display_policy::transition::tests::sleep_does_not_acknowledge_a_pending_change_or_consume_its_resume_budget`

### Optional media attribution and cache policy (8)

Retained selection/pinning/exit behavior, metadata changes with reused track IDs, MPRIS seek bounds, ambiguous/spoofed PWA rejection, paired explicit labels, disabled/offline enrichment, late-result fencing, malformed/unsafe source rejection, bounded transport and DNS/thumbnail protections. Removed exact source-label/content heuristics, browser title/owner attribution projection, per-player mode cleanup, bounded optional process-probe behavior, and enrichment cache/TTL/budget internals. Dedicated valid-label mappings, mode-override expiry, process probing, and request-budget boundaries are reduced.

- `media::browser_identity::pwa::tests::isolated_pwas_survive_chromium_process_title_rewriting`
- `media::browser_identity::pwa::tests::process_reads_are_bounded_optional_and_prefer_explicit_labels`
- `media::browser_identity::tests::dbus_owner_labels_preserve_player_id_and_metadata`
- `media::enrichment::tests::cache_deduplication_negative_ttl_and_request_budget_are_bounded`
- `media::source::tests::exact_services_and_structural_content_hints`
- `media::source::tests::explicit_metadata_wins_without_guessing_from_audio_mime`
- `media::source::tests::unknown_urls_are_preserved_but_never_guessed_or_fetched`
- `media::tests::mode_overrides_are_per_player_and_expire_on_exit`

### Protocol fixture self-checks (4)

Retained the checked contract fixture, every-stream committed-change/resubscription test, live battery/notification/todo API checks, actual sleep dispatch/uncertainty/old-job tests, and state stale-telemetry protection. Removed generated-fixture self-checks and duplicate stale-telemetry wire projection. Audio registry declaration consistency and exhaustive sleep/session fixture examples no longer have their own assertions; production declarations and generated fixture functions are unchanged.

- `protocol::audio_wire::tests::declarations_agree_with_live_reply_constructors_and_registry`
- `protocol::session_control_fixtures::tests::cases_cover_registered_methods_and_selection_uses_real_policy`
- `protocol::sleep_fixtures::tests::live_transitions_distinguish_acceptance_completion_cancellation_and_uncertainty`
- `protocol::sleep_wire::tests::publishing_stale_telemetry_keeps_outcome_and_resume_generation`

### Sleep observation internals (2)

Retained actual lock confirmation/deadline/backpressure tests, authorization/inhibitor refusal, post-setup safety rechecks, sleep outcome ownership/uncertainty, stalled-logind signal/clock recovery, critical-action cancellation/hysteresis, durable policy rollback, and idle trigger changes during setup. Removed dedicated telemetry future-drop/deadline ownership and policy mutex timing tests. Those implementation-level cancellation/locking properties lose direct tests, not the operational sleep safety gates.

- `sleep::monitoring::tests::telemetry_deadline_and_owner_cancellation_drop_pending_work`
- `sleep_policy::tests::slow_observations_do_not_own_or_wait_for_the_policy_mutex`

### State-store benchmark and primitive duplication (3)

Retained real signal initial handoff/burst isolation, stale sleep telemetry fencing, and independent lid ownership. Removed the timing-print baseline, comparison against an obsolete shared ring, and repeated primitive handoff stress loop. There is no new performance claim; the real signal fixture is retained rather than all synthetic repetitions.

- `state::events::reliability_tests::measured_domain_delivery_baseline`
- `state::events::tests::deterministic_shared_ring_baseline_and_domain_isolation`
- `state::tests::watch_handoff_never_misses_a_concurrent_commit`

### Timezone projection (1)

Retained bounded optional location status parsing/clearing and service calendar/DST behavior. Removed named-timezone fresh-projection arithmetic assertions; these are passive projection checks, not a live system timezone-change test. Direct timezone projection coverage is reduced.

- `timezone::tests::fresh_timezone_projection_changes_offset_without_process_restart`

### Update publication and watcher repetition (5)

Retained actual producer atomic candidate publication and crash/reboot/PID-reuse handling, malformed/oversized/symlinked job records, fail-closed candidates, and monitor creation/replacement/removal/later writes. Removed manually staged publication duplication, a second atomic job-status stress/DTO matrix, exact watcher read/identity mechanics, and old-staleness precedence. Dedicated job publication concurrency, missing-watch/identity recovery, self-read suppression, and AI-staleness precedence coverage is reduced. The child-process crash harness test remains registered and usable.

- `updates::jobs::tests::successful_ai_update_clears_only_older_staleness`
- `updates::producer_tests::concurrent_actual_job_status_publication_never_yields_partial_records`
- `updates::tests::interrupted_publication_leaves_old_record_until_commit_or_invalidation`
- `updates::watch::tests::real_reads_do_not_trigger_refresh_but_atomic_writes_do`
- `updates::watch::tests::retries_missing_watch_and_tracks_parent_and_directory_identity`

### Work-area cache lifecycle mechanics (4)

Retained `failed_queries_retry_without_events_then_stop_fast_polling` as a behavioral availability/recovery check. Retired exact shared-cache request counts, one-followup invalidation strategy, slow cache timing, and last-reader future cancellation. These private caching/resource policy properties lose dedicated assertions rather than being presented as fully covered elsewhere.

- `work_area::tests::dropping_last_reader_cancels_inflight_query`
- `work_area::tests::healthy_cache_is_shared_event_driven_and_retained_until_invalidated`
- `work_area::tests::invalidation_during_a_read_triggers_one_followup`
- `work_area::tests::silent_changes_reconcile_slowly_and_old_idle_cache_refreshes_on_demand`
