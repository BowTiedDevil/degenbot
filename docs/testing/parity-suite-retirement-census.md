# Population 1 parity-suite retirement census

Status: retired. The three hand-written pool-behavior parity suites below
pushed synthetic events into pool objects and asserted the resulting state
transitions. That behavior lives in the Rust core now, so those tests were
second copies of Rust assertions maintained against a Python seam that is only
a driver. This document is the census that replaced them: every deleted Python
test, the assertions it made, and the Rust test that owns each assertion.

Scope (Population 1 of the Python parity suites):

- `tests/uniswap/v3/test_uniswap_v3_liquidity_pool.py`
- `tests/uniswap/v2/test_uniswap_v2_liquidity_pool.py`
- `tests/aerodrome/test_aerodrome_pools.py`

Selection rule: a test was deleted when it called one of the write-driving
seams on a pool object — `external_update`, `update_liquidity_map`,
`update_tick_data`, `discard_states_before_block`,
`restore_state_before_block`. Read-only tests (creation, pricing/rate,
`calculate_*`, simulations, deployment identity, address generation) were left
untouched.

## Method

For each write-driving test, every assertion was traced to the Rust test that
asserts the same fact, and the Rust test is named here as
`crate :: module path :: test name`. A test file's fork fixture was checked for
survival after deletion (a vestigial fork fixture keeps a test in the fork
lane even when the test body no longer needs a chain).

Two assertion classes needed care:

- **Adapter-layer guards.** The Python companions (`external_update` on
  `ConcentratedLiquidityCompanion` / V2 / Aerodrome pools) refuse a
  backward-stamped observation with `ExternalUpdateError` before delegating.
  That refusal is the shell's, and its Rust twin is the core's own
  backward-stamp policy: the journal drops an older-block delta
  (`push_delta_older_block_rejected`) and the refresh entry declines an
  observation at/below the pool clock (`RefreshOutcome::Unchanged`).
- **The refresh compare half.** `Bot.update` delegates to the core refresh
  entry (`Pool.refresh_via` -> `degenbot_bot::bot_core::refresh::refresh_pool`),
  whose fetch/compare/apply choreography had Rust tests only for its refusal
  paths. One Rust test was written before deleting the Python test that
  covered it:
  `degenbot-bot :: bot_core::refresh::tests :: a_differing_reserve_pair_is_applied_and_a_matching_one_is_unchanged`
  (a differing fetched pair lands and reports `Changed`; a matching pair and an
  observation at/below the pool clock report `Unchanged` and move nothing).

## Counts

| File | Deleted | Migrated | Remaining (read-only) |
| --- | --- | --- | --- |
| `tests/uniswap/v3/test_uniswap_v3_liquidity_pool.py` | 9 | 0 | 20 |
| `tests/uniswap/v2/test_uniswap_v2_liquidity_pool.py` | 5 | 0 | 16 |
| `tests/aerodrome/test_aerodrome_pools.py` | 2 | 0 | 5 |

"Migrated" counts a Python test rewritten against a surviving seam. Every test
here deleted instead: the assertion moved to Rust, not to a rewritten Python
test. No write-driving test survives, so no remaining test needs a census row.

## `tests/uniswap/v3/test_uniswap_v3_liquidity_pool.py`

| Deleted test | Assertion(s) it made | Rust coverage |
| --- | --- | --- |
| `test_external_update_with_sparse_liquidity_map` | A Sparse-coverage pool reports `sparse_liquidity_map` and has no bitmap entry for an unfetched word | `degenbot-pools :: registry::projection_tests :: coverage_reader_reports_registration_coverage`; `degenbot-substrate :: tests::quarantine :: fresh_pool_lifecycle_is_coverage_aware` |
| | `update_liquidity_map` on a Sparse pool whose boundary word is unknown backfills the word (no `LiquidityMapWordMissing`) | `degenbot-substrate :: tests::word_fetch :: ensure_word_known_merges_ticks_and_marks_word_known`; `ensure_word_known_checked_empty_marks_word_known`; refusal twins `ensure_word_known_no_fetcher_returns_false`, `ensure_word_known_fetch_error_returns_false` |
| | the event still applies to the tick map (out-of-range region) | `degenbot-pools :: families::concentrated_liquidity::state_journal::tests :: liquidity_update_out_of_range_is_tick_only_and_journals_no_scalars_on_both_adapters` |
| `test_reorg` | ten `external_update` Swap writes land, each stamping `update_block` | `degenbot-pools :: families::concentrated_liquidity::state_journal::tests :: restore_writes_landed_at_scalars_and_restore_point_block_on_both_adapters` (swap advances both clocks); `degenbot-substrate :: tests::apply_routes :: apply_pump_buffer_v3_journals_and_advances_update_block` |
| | `restore_state_before_block(newest + 1)` is a no-op that leaves the landed state alone | `degenbot-pools :: families::concentrated_liquidity::state_journal::tests :: restore_no_op_when_newest_delta_before_target_on_both_adapters`; `degenbot-substrate :: tests::apply_routes :: v3_restore_before_block_past_newest_leaves_state_untouched` |
| | unwinding block by block lands the stored pre-block scalars (liquidity / sqrt_price / tick) | `degenbot-pools :: families::concentrated_liquidity::state_journal::tests :: multi_block_popped_range_rewinds_to_the_pre_target_state_on_both_adapters`; `double_rollback_equals_a_single_rollback_to_the_earlier_target_on_both_adapters`; `rewind_equals_replay_on_both_adapters` (property) |
| | the fully unwound pool equals its starting scalars | `degenbot-pools :: families::concentrated_liquidity::state_journal::tests :: rewind_at_registration_and_one_past_it_are_legal_on_both_adapters` |
| `test_discard_before_finalized` | eleven Swap writes, then `discard_states_before_block(finalized)` trims history only: the live state and both clocks are untouched | `degenbot-pools :: families::concentrated_liquidity::state_journal::tests :: discard_drops_old_deltas_without_mutating_live_state_on_both_adapters`; journal trim semantics `degenbot-pools :: state_history::tests :: discard_removes_deltas_before_target_keeping_rest` |
| `test_discard_earlier_than_created` | a discard target before the registration snapshot is a no-op; `update_block` and `state` are unchanged | `degenbot-pools :: state_history::tests :: discard_noops_when_earliest_at_or_after_target`; `discard_noops_on_empty_journal`; live-state half `...state_journal::tests :: discard_drops_old_deltas_without_mutating_live_state_on_both_adapters` |
| `test_discard_after_last_update` | discarding past the newest delta raises `NoPoolStateAvailable` ("No pool state known prior to block B") and mutates nothing | `degenbot-pools :: state_history::tests :: discard_errors_when_target_past_newest` |
| `test_external_update` | a Swap write at `update_block + 1` lands and stamps `update_block` | `degenbot-pools :: families::concentrated_liquidity::state_journal::tests :: restore_writes_landed_at_scalars_and_restore_point_block_on_both_adapters`; `degenbot-substrate :: tests::apply_routes :: apply_pump_buffer_v3_journals_and_advances_update_block` |
| | a Mint applies `gross += delta` at both boundary ticks, `net += delta` at the lower and `net -= delta` at the upper, against real mainnet tick rows | `degenbot-pools :: families::concentrated_liquidity::state_journal::tests :: liquidity_update_out_of_range_is_tick_only_and_journals_no_scalars_on_both_adapters`; concrete values `degenbot-substrate :: tests::apply_routes :: apply_backfill_buffer_v3_journals_and_advances_update_block` |
| | a backward-stamped `external_update` is refused (`ExternalUpdateError`) | `degenbot-pools :: state_history::tests :: push_delta_older_block_rejected` (older-block delta stays out of history); shell refusal twin `degenbot-bot :: bot_core::refresh::tests :: a_differing_reserve_pair_is_applied_and_a_matching_one_is_unchanged` |
| | a second in-range Mint at [257880, 257940) with the current tick inside raises the active `liquidity` scalar by exactly the delta | `degenbot-pools :: families::concentrated_liquidity::state_journal::tests :: liquidity_update_in_range_mint_adjusts_active_liquidity_and_restores_on_both_adapters` |
| | a repeated identical `external_update` at the same block is a no-change write | shell short-circuit (`False`, no core call); core twin for the same-block stamp `degenbot-pools :: state_history::tests :: push_delta_same_block_coalesces_keeping_earliest_scalar_priors` and the refresh `Unchanged` arm (same test as above) |
| `test_mint_and_burn_in_empty_word` | a Mint inside an empty bitmap word materializes both boundary ticks and makes the word present | `degenbot-pools :: tick_bitmap::tests :: flip_equivalence_apply_maintains_bit_row_consistency` |
| | the matching Burn removes both tick rows once `liquidity_gross` returns to zero | `degenbot-pools :: tick_bitmap::tests :: test_apply_liquidity_removes_touched_tick_when_gross_zero` |
| `test_complex_liquidity_transaction_1` | Burn -> Swap -> Mint over the current tick sums the active `liquidity` scalar (474..89) | `...state_journal::tests :: liquidity_update_in_range_burn_reduces_active_liquidity_on_both_adapters`; `liquidity_update_in_range_mint_adjusts_active_liquidity_and_restores_on_both_adapters` |
| | per-tick `liquidity_gross` / `liquidity_net` after the sequence, including nets whose signed value only reads correctly through the unsigned 2^128 form | `...state_journal::tests :: liquidity_update_out_of_range_is_tick_only_and_journals_no_scalars_on_both_adapters` (net sign convention); wide-delta range `degenbot-substrate :: tests::apply_routes :: v4_liquidity_delta_applies_at_the_int128_boundaries`; concrete gross/net `degenbot-substrate :: tests::apply_routes :: apply_liquidity_update_by_pool_id_routes_to_v4_and_applies_ticks` |
| `test_complex_liquidity_transaction_2` | same shape, different position set and triple (477..61) | same rows as `test_complex_liquidity_transaction_1` |

## `tests/uniswap/v2/test_uniswap_v2_liquidity_pool.py`

| Deleted test | Assertion(s) it made | Rust coverage |
| --- | --- | --- |
| `test_reorg` | ten `external_update` Sync writes land, each stamping `update_block == block_number` | `degenbot-pools :: v2_state::apply_inherent_tests :: apply_sync_updates_reserves_advances_block_and_journals_priors` |
| | `restore_state_before_block(0)` raises `NoPoolStateAvailable` | `degenbot-pools :: state_history::tests :: restore_errors_when_target_at_or_before_genesis`; `restore_errors_on_empty_journal` |
| | `restore_state_before_block(last + 1)` is a no-op returning the current state | `degenbot-pools :: state_history::tests :: restore_noop_returns_current_when_newest_below_target` |
| | each unwind lands the stored pre-block state | `degenbot-pools :: state_history::tests :: restore_lands_at_largest_block_below_target`; `restore_intermediate_lands_between_deltas` |
| | the fully unwound pool equals its starting state | `degenbot-pools :: state_history::tests :: restore_to_first_update_lands_at_genesis`; trait surface `degenbot-pools tests/reorg_pool_state_trait.rs :: v2_restore_before_block_returns_unit_and_writes_landed_at_fields` |
| `test_discard_before_finalized` | eleven Sync writes, then `discard_states_before_block(finalized)` leaves `update_block` at the finalized head and the state intact | `degenbot-pools :: state_history::tests :: discard_removes_deltas_before_target_keeping_rest`; clock half `degenbot-pools :: v2_state::apply_inherent_tests :: apply_sync_updates_reserves_advances_block_and_journals_priors` |
| `test_discard_earlier_than_created` | a discard target below registration leaves the journal length unchanged | `degenbot-pools :: state_history::tests :: discard_noops_when_earliest_at_or_after_target`; `discard_noops_on_empty_journal`; length reader `...families::concentrated_liquidity::state_journal::tests :: journal_len_reports_delta_count_on_both_adapters` |
| `test_discard_after_last_update` | discarding past the newest delta raises `NoPoolStateAvailable` and mutates nothing | `degenbot-pools :: state_history::tests :: discard_errors_when_target_past_newest` |
| `test_late_update` | a Sync write at a block below `update_block` is refused (`ExternalUpdateError`) | journal refusal `degenbot-pools :: state_history::tests :: push_delta_older_block_rejected`; the core refresh entry declines an observation at/below the pool clock `degenbot-bot :: bot_core::refresh::tests :: a_differing_reserve_pair_is_applied_and_a_matching_one_is_unchanged` |
| | a duplicate write for the newest block is accepted | `degenbot-pools :: v2_state::apply_inherent_tests :: apply_sync_same_block_replaces_newest_delta`; `degenbot-pools :: state_history::tests :: push_delta_same_block_replaces` |

## `tests/aerodrome/test_aerodrome_pools.py`

| Deleted test | Assertion(s) it made | Rust coverage |
| --- | --- | --- |
| `test_bot_update_state` | a reserve write through the external-update seam at the pool's current block lands (same-block replace, no refusal) | `degenbot-pools :: aerodrome_v2_state::apply_inherent_tests :: apply_sync_updates_reserves_advances_block_and_journals_priors`; `degenbot-pools :: state_history::tests :: push_delta_same_block_replaces` |
| | `bot.update(pool)` reports `True` when the fetched on-chain reserves differ, and the pool state is reconciled | `degenbot-bot :: bot_core::refresh::tests :: a_differing_reserve_pair_is_applied_and_a_matching_one_is_unchanged` (written for this deletion) |
| | a second `bot.update(pool)` at the same state reports `False` | same test (`RefreshOutcome::Unchanged` arms) |
| `test_external_update` | a reserve write at `update_block + 1` lands and the state reports the new reserves | `degenbot-pools :: aerodrome_v2_state::apply_inherent_tests :: apply_sync_updates_reserves_advances_block_and_journals_priors`; end to end through the dispatcher `degenbot-substrate :: tests::apply_routes :: aerodrome_reserve_mutation_and_reorg_rollback` |
| | a second write for the same block with the current reserves is accepted | `degenbot-pools :: state_history::tests :: push_delta_same_block_replaces`; `degenbot-pools :: v2_state::apply_inherent_tests :: apply_sync_same_block_replaces_newest_delta` (shared journal) |
| | a write ten blocks in the past raises `ExternalUpdateError` | journal refusal `degenbot-pools :: state_history::tests :: push_delta_older_block_rejected`; core refresh `Unchanged` arm (same test as the row above) |

## Surviving population

The read-only tests stay in their files, on their existing fixtures:

- **V3 (20):** `test_first_200_pools`, `test_pool_creation`,
  `test_pool_creation_with_liquidity_map`,
  `test_built_pool_identity_matches_deployment_truth`,
  `test_pancake_v3_pool_creation`, `test_sparse_liquidity_map`,
  `test_tick_data_equality`, the three price/rate tests, the four
  `calculate_*` tests, `test_simulations`, `test_simulations_with_override`,
  `test_simulation_input_validation`, `test_zero_swaps`, `test_swap_for_all`,
  `test_base_pancakeswap_v3`.
- **V2 (16):** the three price/rate tests, the two Camelot creation tests
  (one `skip`), `test_dunder_methods`, the four `calculate_*` tests,
  `test_comparisons`, `test_simulations`, `test_simulations_with_override`,
  `test_simulation_input_validation`, `test_zero_swaps`, `test_swap_for_all`.
- **Aerodrome (5):** `test_aerodrome_v2_address_generator`,
  `test_create_pool`, `test_aerodrome_v3_pool_creation`,
  `test_aerodrome_v3_state`, `test_aerodrome_v3_pool_calculation`.

Fixtures used only by the deleted tests were removed with them
(`convert_unsigned_integer_to_signed` in the V3 file, the V3/V2 external-update
and liquidity-map update types, `NoPoolStateAvailable` /
`ExternalUpdateError` imports, the V2 `TYPE_CHECKING` `BlockNumber` import).
The surviving tests' fixtures — the recorded-golden WBTC/WETH pools,
`fork_mainnet_full`, `fork_base_full`, `fork_arbitrum_full`, the 200-pool
fixture, `dai` — are untouched.

The deleted fork-lane tests were the V3 file's `fork_mainnet_archive`
parametrizations (`test_reorg` at 17,600,000, `test_external_update` at
17,600,000, `test_mint_and_burn_in_empty_word` at 20,751,740,
`test_complex_liquidity_transaction_1/2` at 19,619,258 / 19,624,318). The
`fork_mainnet_archive` fixture keeps other users
(`tests/curve/test_curve_stableswap_pool.py`,
`tests/balancer/test_stable_pool_class.py`,
`tests/balancer/test_stable_pools.py`), so it stays in `tests/conftest.py`;
the V3 file keeps its `online_rpc` coverage through
`test_sparse_liquidity_map` and its `slow` 200-pool quoter sweep.
