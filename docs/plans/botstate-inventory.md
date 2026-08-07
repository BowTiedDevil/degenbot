# T1 — `impl BotState` method classification (move-cut artifact)

Source: `rust/crates/degenbot-bot/src/bot_core/mod.rs`, `impl BotState` (lines 594–4363).
Classification uses CONTEXT.md's three structural families. Decision per method:
`MOVE-CL` (this epic's pilot, T2), `MOVE-RP` (reserve-pair, T5), `MOVE-BV` (balance-vector, T5),
`STAY` (resident in mod.rs). Assignments are the authoritative cut for the move tasks.

## Registry / identity / construction — STAY
new, with_journal_depth, pool_entry, pool_state_nonce, pool_update_block, pool_state_head,
pool_tick_data_block, set_pump_complete_cutoff, pump_complete_cutoff, advance_pump_complete_cutoff,
pool_id_by_address, unregister_pool, pool_count, has_pool, has_token, token_entry, register_token,
pool_family

## Cross-family reorg dispatch (ADR-016 unified) — STAY
restore_all_pools_before_block, restore_pool_before_block, discard_pool_before_block,
pool_journal_len, has_state_prior_to  (match all 7 PoolEntry variants; NOT a V3-only journal method)

## Cross-family pool-action dispatch — STAY
apply_swap_by_pool_id, seed_genesis_by_pool_id, apply_liquidity_update_by_pool_id

## Solver-facing calc / encode / event router — STAY (deferred carve-out)
calculate_tokens_out_miss_aware, calculate_tokens_out_with_fetch, calculate_tokens_in,
simulate_exact_input_swap_miss_aware, simulate_exact_input_swap_with_fetch,
simulate_exact_output_swap_miss_aware, simulate_exact_output_swap_with_fetch,
simulate_swap_with_override, encode_swap, process_backfill_logs, get_v3_or_v4_pool
(Q9 decision: `get_v3_or_v4_pool` stays resident with the solver-calc that consumes it —
resolves the Q9/Q10 tension; it is a CL read used by the resident simulate/calc layer.)

## CL slice — MOVE-CL (T2) → `bot_core/cl_orchestration.rs`

### V3 family
register_v3_pool, update_v3_pool, apply_v3_swap, apply_v3_swap_by_pool_id,
apply_v3_liquidity_update, apply_v3_liquidity_update_by_pool_id, sync_tick_data_by_pool_id,
sync_v3_pool_state, merge_tick_word, get_v3_pool, get_v3_identity, v3_pools_snapshot, v3_pool_count

### V4 family
register_v4_state_view, state_view_for, register_v4_pool, apply_v4_swap,
apply_v4_liquidity_update, apply_v4_swap_by_pool_id, apply_v4_liquidity_update_by_pool_id,
get_v4_pool, get_v4_identity, v4_pool_id_by_key, v4_pool_count, v4_registered_pool_managers,
v4_pools_snapshot, sync_v4_pool_state

### CL-common dual-buffer (V3+V4 twins — move together, never split a twin)
buffer_backfill_v3_liquidity_update, apply_backfill_buffer_v3, apply_pump_buffer_v3,
buffered_v3_event_count, buffered_v4_event_count, flush_v3_buffer, expire_v3_buffered,
apply_buffered_v3_event (private helper), buffer_backfill_v4_liquidity_update,
apply_backfill_buffer_v4, apply_pump_buffer_v4, set_v4_buffer_max_age, flush_v4_buffer,
expire_v4_buffered, apply_buffered_v4_event (private helper)

### CL-common snapshot
set_snapshot_seed_block, v3_snapshot_seed, take_v3_snapshot_seed, pin_v3_post_drain_snapshot,
take_v3_post_drain_snapshot, v4_snapshot_seed, take_v4_snapshot_seed, pin_v4_post_drain_snapshot,
take_v4_post_drain_snapshot

### CL-common coverage/quarantine/lifecycle state accessors (ADR-022 read side; sequence stays in registration_lifecycle.rs)
v3_pool_coverage, v4_pool_coverage, set_v3_pool_quarantined, set_v4_pool_quarantined,
set_v3_pool_live, set_v4_pool_live, release_all_v3_v4_quarantined

## Reserve-pair slice — MOVE-RP (T5) → `reserve_pair_orchestration.rs`
register_v2_pool, apply_v2_sync, update_v2_pool, apply_sync_by_pool_id, get_v2_pool_state,
get_v2_identity, v2_snapshot, v2_pool_count, register_aerodrome_pool,
get_aerodrome_identity, get_aerodrome_pool

## Balance-vector slice — MOVE-BV (T5) → `balance_vector_orchestration.rs`
register_curve_pool, apply_balance_update_by_pool_id, get_curve_pool, get_curve_identity,
curve_get_dy, curve_get_dy_underlying, curve_calc_token_amount, curve_calc_withdraw_one_coin,
register_balancer_weighted_pool, get_balancer_weighted_pool, get_balancer_weighted_identity,
register_balancer_stable_pool, get_balancer_stable_pool, get_balancer_stable_identity

## Manual corrections against the plan doc (note for D6 / plan doc update)
- `has_state_prior_to` (and the whole reorg block) is CROSS-FAMILY (matches all 7 variants), NOT a
  "V3 journal method" — stays resident. The plan doc's phrase "the V3 journal methods" is dropped.
- `get_v3_or_v4_pool` stays resident per Q9 (named decision), resolving the Q9/Q10 overlap.
- The grid `v3_pool_count`/`v4_pool_count`/`v2_pool_count` move WITH their family slices (keeps the
  CL/reserve-pair/bv modules internally consistent), rather than staying as resident registry counters.
