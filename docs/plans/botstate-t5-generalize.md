# T5 — Generalize: reserve-pair + balance-vector slices (done)

Same RED-neutral→GREEN discipline as the CL pilot (T2), applied to the other two
CONTEXT structural families, each as its own sibling module:

| Slice | Methods | Module | Gate |
|-------|---------|--------|------|
| reserve-pair (`V2` + `AerodromeV2`) | 11 (`register_v2_pool`, `apply_v2_sync`, `update_v2_pool`, `apply_sync_by_pool_id`, `get_v2_pool_state`, `get_v2_identity`, `v2_snapshot`, `v2_pool_count`, `register_aerodrome_pool`, `get_aerodrome_identity`, `get_aerodrome_pool`) | `reserve_pair_orchestration.rs` | 410 tests green |
| balance-vector (`Curve` + `Balancer` weighted/stable) | 14 (`register_curve_pool`, `apply_balance_update_by_pool_id`, `get_curve_pool`, `get_curve_identity`, `curve_get_dy(_underlying)`, `curve_calc_token_amount`, `curve_calc_withdraw_one_coin`, `register_balancer_{weighted,stable}_pool`, `get_balancer_{weighted,stable}_pool(_identity)`) + 2 consts (`CURVE_FEE_DENOMINATOR`, `CURVE_PRECISION`) + 4 sole-user free fns (`curve_base_xp`, `curve_ramping_params`, `curve_block_timestamp`, `curve_total_supply`) | `balance_vector_orchestration.rs` | 410 tests green |

## Extraction-correctness note
`extract_general.py` (comment/string-aware brace extractor) initially mis-parsed a
stay-resident method (`pool_family`, which returns `&'static str`): the neutralizer
treated `'` as a char-literal start, so the lifetime `'static` opened an unterminated
char-state that erased the method's closing `}` and swallowed the following
`get_v2_identity`. Fixed by only treating `'` as char-literal when it is `'X'` or `'\x'`
(lifetime detection preserved). Re-extraction then removed exactly 11 reserve-pair
methods and 14 balance-vector methods from `mod.rs`; each removal was verified with
`grep` (0 residual impl methods) rather than trusted from the script.

## Import wiring
- `reserve_pair_orchestration`: state/identity/params from `degenbot_pools`
  (`v2_state`, `aerodrome_v2_state`), `aliases::U112` (V2 reserves are `u112`), and
  `super::{BotState, PoolEntry}`.
- `balance_vector_orchestration`: `degenbot_curve_math` calc/ramp helpers
  (`calculate_dy`, `calculate_dy_underlying`, `resolve_ramping_a`, `stableswap_get_d`,
  `stableswap_get_y_d`, `DVariant`, `YDVariant`, `CurveSwapError`, `ARampingParams`);
  `degenbot_pools` balance/curve state (`balancer_{stable,weighted}_state`, `curve_state`);
  and `super` for `BotState`, `PoolEntry`, `BotCurveBasePoolPort`, `CurveInputsError`,
  `resolve_dy_inputs`. The `_dy`/`_underlying`/calc methods route through
  `BotCurveBasePoolPort` (a `pub(crate)` mod.rs struct) for metapool base delegation,
  and the 4 curve helper fns + 2 consts moved here as sole-user helpers with their family.
- `mod.rs` trimmed the now-orphaned imports (`DVariant`, `YDVariant`, the `calculate_*`/
  `stableswap_*` curve-math fns, `ARampingParams`, `U112`).

## What stayed resident in mod.rs (deliberate)
Cross-family registry/reorg dispatch (`pool_family`, `pool_id_by_address`, `unregister`,
`restore_*`, `apply_swap_by_pool_id`, `apply_liquidity_update_by_pool_id`,
`get_v3_or_v4_pool`) and the solver-facing calc/`simulate_*`/`encode_swap` CLI, plus the
`BotCurveBasePoolPort` struct/impl (which delegates to the moved inherent methods).
Test module stays resident (T3 decision).

## Gates (all green)
`cargo test -p degenbot-bot` → 410 pass · `cargo clippy -p degenbot-bot --all-targets
-- --deny warnings` → clean · `cargo fmt -p degenbot-bot --check` → clean ·
`just test-rust` (workspace + standalone) → 111 suites, 0 failures.
