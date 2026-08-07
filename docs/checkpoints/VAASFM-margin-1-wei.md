# VAASFM Checkpoint — Clamp margin decision

**Task:** VAASFM — Wire the solver CL-hop clamp (the path-5000 fix)

**Decision:** Clamp margin = **1 wei** (user-approved, maximum extraction). The
committed CL-hop input is `max_convertible - 1` (the tier-3-proven
`v4_simulate_swap.input_consumed` minus 1 wei), so the exact-in loop converts
nearly everything and stops on `amountRemaining == 0` at the last funded tick.
A larger margin can be revisited if runaway swaps recur.

## Clamped solve result for the path-5000 fixture (block 25704509)

V2(MATIC/WETH) → V4(UNI/MATIC) → V3(UNI/WETH), V4 hop = 1, fee=100, spacing=1,
zfo=false.

| quantity | unclamped (recorded) | clamped (margin=1) |
|---|---|---|
| V4 committed input | `15351327867212777` | `15351327867192637` (`max_convertible - 1`) |
| `max_convertible` (`input_consumed`) | — | `15351327867192638` (leftover 20,139) |
| V4 output | `460882096151249` | `460882096151249` (**byte-identical**) |
| real-PM gas (default MAX limit) | **20,776,614** → EMPTY-HALT | **190,755** |
| loop ends at tick | 887271 (liq→0) | 35066 |

The 1-wei margin yields zero extraction loss (clamped output == solver output),
unlike the earlier 21,000 wei demo which cost a ~630-wei output reduction.

## Files changed (investigation / demo layer — production wiring pending)

- `rust/crates/degenbot/src/investigation/hop_oracle.rs` — `v4_hop_output_consumed()`
  (returns `OracleWithConsumed { outcome, input_consumed }`), `OracleOutcome::is_ok()`.
- `rust/crates/degenbot/src/investigation/mod.rs` — re-exports.
- `rust/crates/degenbot/examples/path5000_v2v4v3_solver_fixture.rs` — margin const
  (`CLAMP_MARGIN` env overridable, default 1 wei) + the VAASFM clamp-result section.

## Production placement (open decision)

The authoritative clamp bound comes from `v4_simulate_swap.input_consumed`, which
needs `V4PoolState` — not reachable in the pure `degenbot-solvers` crate (it only
carries `IntV3TickRangeSequence`). Recommended production seam: `degenbot-bot`
`arb_engine` post-solve (option A; `BotState` has `get_v4_pool`/`get_v3_pool` and
`v4_simulate_swap` is re-exported). Option B (thread pool state into the solver)
is a larger API change. Owned by the follow-on wiring task.
