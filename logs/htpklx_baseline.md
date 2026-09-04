# HTPKLX baseline: pool-lifecycle + solve-result heap tracking

Epic: HTPKLX (Rust-core memory layout optimization epilogue). Task: KKNKVS.
Date: 2026-09-04. Baseline tree: 9ec8d5398 (Box<[T]> for never-mutated Vecs
in pool state/identity + swap-update tick_priors + StaticRateProvider.rates).

## Method

Custom `GlobalAlloc` counting shim over the system allocator, activated only
inside a `measure()` window; opt-in gate `DEGENBOT_ALLOC_TRACK=1` (off by
default, matching the DEGENBOT_HOTPATH runtime-gate convention). Harnesses:

- `rust/crates/degenbot-pools/tests/alloc_tracking.rs` — pool lifecycle (all
  phases run in one single-threaded `#[test]` so cross-thread noise cannot
  skew counts).
- `rust/crates/degenbot-math/tests/alloc_tracking.rs` — solve-result
  build/clone (baseline for the step-outcome merge, task 4JLQNS).

Static sizes pinned by `rust/crates/degenbot-pools/tests/size_fidelity.rs` —
any future drift turns that test red (it blocked `panic!` const formatting,
so only `TickInfo` carries a hard pin today and the rest report under
`--nocapture`; pin hardening is cheap follow-up as sizes change).

## Static sizes (fidelity pins + report)

| type | bytes |
|---|---|
| TickInfo | 56 (pinned) |
| PoolEntry (inline enum) | **512** |
| V2PoolState | 88 |
| V3PoolState | 368 |
| V4PoolState | 384 |
| CurvePoolState | 88 |
| BalancerWeightedPoolState | 72 |
| BalancerStablePoolState | 88 |
| CurvePoolIdentity | 400 |
| BalancerWeightedPoolIdentity | 144 |
| BalancerStablePoolIdentity | 160 |

PoolEntry at 512 B vs V2PoolState at 88 B: every V2/Aerodrome registry slot
pays V3-state-class width — the boxing task (KO3SBO) quantitative case.

## Per-op heap measurements

| op | bytes | allocs |
|---|---|---|
| v3_register_128_ticks (from_params, journal depth 8) | 34,465 | 4 |
| v3_state_clone_128_ticks | 16,656 | 1 |
| v3_apply_swap_empty_priors | 0 | 0 |
| tick_map_build_128 | 16,656 | 1 |
| v2_register_journal8 | 576 | 1 |
| v2_state_clone | 72 | 1 |
| simulation_result_build_hops2 | 128 | 2 |
| simulation_result_build_hops3 | 192 | 2 |
| simulation_result_build_hops5 | 320 | 2 |
| simulation_result_clone_hops3 | 192 | 2 |

Per-tick read: ~130 B/tick for a 128-entry hashbrown tick map (56 B payload +
map overhead+journal-free clone). Per-result read: 2 allocs for the two
parallel per-hop Vecs regardless of hop count (32 B/hop) — the step-outcome
merge halves that to 1 alloc and shrinks per-hop bytes.

## Reproduce

    cd rust && DEGENBOT_ALLOC_TRACK=1 cargo test -p degenbot-pools \
      --test alloc_tracking -- --nocapture --test-threads=1
    cd rust && DEGENBOT_ALLOC_TRACK=1 cargo test -p degenbot-math \
      --test alloc_tracking -- --nocapture --test-threads=1

## Verification at authoring time

`cargo test -p degenbot-pools -p degenbot-math` all green (incl. the 2 new
harness/fidelity bins); fmt-check clean. clippy run on touched crates with
zero visible errors. See task result note for the exact counts.
## Post-merge update (task 4JLQNS continuation + TQBBCO)

After commit b20be5ad5 (SimulationResult) + the TQBBCO merge (DispatchCandidate/SimulatePath -> Box<[SolveStep]>, SolveStep{output, consumed_input, state_nonce}):

| op | bytes | allocs | delta vs baseline |
|---|---|---|---|
| simulation_result_build_hops3 | 192 | **1** | was 2 |
| simulation_result_build_hops5 | 320 | **1** | was 2 |
| simulation_result_clone_hops3 | 192 | **1** | was 2 |
| v3 tick/clone overhead | unchanged | unchanged | — |

Per-simulated-path allocation count on the solve hot loop halved; SimulatePath/DispatchCandidate each drop two heap allocations + 48 B of struct headers per candidate.

Executor encode path intentionally NOT merged: EncodeRequest is built per submission (cold), and `grammar_shape` re-borrows `inputs.consumed_inputs = &CLAMPED` during plan construction — the two-slice borrow is load-bearing there; conversion would fight the plan-time clamp with no per-cycle win. SolveResult (execution) still exposed as parallel Vecs pending the PyO3 accessor slice; its merge is the LIBQKE-adjacent follow-up in this task's scope deferred until the next slice unless the numbers demand otherwise (recorded in the task result note).

## KO3SBO measured checkpoint data (2026-09-04, pre-conversion)

- PoolEntry (inline enum) = 512 B (pinned by tests/size_fidelity.rs report).
- V2PoolState = 88 B, AerodromeV2 state similar; V3PoolState = 368 B, V4 384 B.
- A V2/Aerodrome registry slot therefore wastes ~424 B vs its own payload:
  ~424 B x live reserve-pair pool count is the boxing win; every V3/V4 slot
  is unchanged in memory (their width IS the enum's width).
- Conversion approach (validated to compile at crate level before the
  mid-flight pause): PoolEntry::Fam(Box<(VxPoolIdentity, VxPoolState)>);
  the ADR-014 D5 vN()/vN_mut() projections are the single adaptation seam
  (already proven: p.0/p.1 derefs compile through borrow AND mut variants).
- Indirection cost: one deref inside the registry-probe path (already behind
  the HashMap lookup); P99 solve impact expected negligible but the go/no-go
  re-runs the z2 solver benches at resume (the task's checkpoint gate).
- Resume scope (uncommitted remainder when paused): ~30 two-field pattern
  sites in bot_core (swap_simulation.rs 26, cl_orchestration, block_pump,
  divergence_probe, mod.rs, the three balance_vector_orchestration
  .insert(PoolEntry::Fam(identity, state)) sites, reserve_pair_orchestration,
  pool_builder), python bot/mod.rs (4), pools registry 1 test literal.
  Each site converts PoolEntry::Fam(id, st) -> PoolEntry::Fam(p) plus
  p.0/p.1 field paths (mut variants take &mut p.0, &mut p.1); constructor
  sites get Box::new((id, st)).
- PAUSE REASON: the peer agent is actively committing to bot_core (DO5Q5E
  follow-ups: event_dispatch.rs, failure_policy docs, 3 commits this
  session); a ~30-site bot_core churn landing over their WIP risks cross-
  agent conflict. Resume after their burst lands.

## KO3SBO measured checkpoint (2026-09-04, post-boxing)

- PoolEntry: **512 B -> 16 B** (pointer+tag; now pinned by
  `pool_entry_size_pinned` in pools/tests/size_fidelity.rs, citing KO3SBO).
- Per-variant payload widths unchanged (V2 88, V3 368, V4 384, Curve 88,
  BalW 72, BalS 88 B) — boxing moved them to the heap, one pair-alloc per
  pool registration (amortized over the pool's lifetime, not per-op).
- Registry cache footprint: 10k pools x (512-16) ~ 4.8 MB of hot-map bytes
  dropped; the V2/Aerodrome fleet dominated the story (unordered lookup is
  now pointer-chase after the map hop instead of a 512 B slab per slot).
- Lookup latency: no registry-probe bench existed pre-change; the fit-for-
  purpose estimate from the KKNVVS harness lifecycle (one HashMap probe +
  one projection) is unchanged in op count (+1 deref). Solver digest bench
  post-change: solve_v2/v3 paths 1.04-1.63 us, solve_curve_path 156 us
  (baseline for the TZSWOP consolidated report).
- Go/no-go: GO (latency is flat by construction for the probe; the risk
  path — per-swap registry lookups — touches the map once, and the map's
  footprint shrank 32x per entry, favoring cache).
- Conversion: vN()/vN_mut() projections kept their reference signatures;
  let-else/if-let sites rewritten through `.and_then(PoolEntry::vN_mut)
  .map(|(_, s)| s)` (single-line, no block surgery).

## LIBQKE storage finding (2026-09-04, pre-conversion checkpoint)

- liquidity_net persists in degenbot-db as **signed-decimal TEXT** (VARCHAR(78)
  column, LiquidityTar之后的 `IntMappedToString` mapping): `encode_i256` =
  `I256::to_string()`, `decode_i256` = `I256::from_dec_str(s.trim())`
  (liquidity_updater.rs:1371-1381, rows/decode.rs:25).
- i128's signed-decimal text is byte-identical to the same value's I256 text
  for every in-range value, so an i128-typed field (encoded via
  `i128::to_string()`, decoded via `i128::from_str`) round-trips byte-
  identically over the existing column: **no migration needed**.
- Out-of-range rows: the current Rust reader already narrows I256->i128 via
  the low-16-byte projection (liquidity_net_i128); a >127-bit row would be
  lossy today and fails to decode under i128 — decode-time hard failure beats
  silent wraparound. Values originate from the on-chain int128 field, so rows
  are in-range by construction.
- Decision: proceed (no-migration path confirmed).

## LIBQKE final measured results (2026-09-04)

- TickInfo: 56 B -> **48 B** (16 gross + 16 net + 8 block, pad-to-16). The
  task body projected 40 B; the 8 B come from struct alignment to U128's 16 B
  (the fleet still wins 8 B/tick on the dominant HashMap<i32, TickInfo>).
- Pin updated in pools/tests/size_fidelity.rs citing LIBQKE; PoolEntry=16 B
  (KO3SBO) and TickInfo=48 B pins both green.
- Validation: cargo test across pools/math/db/bot/solvers/simulation/rpc/
  pool-updater/degenbot -> **2774 passed, 0 failed** (workspace-wide cargo
  test --workspace: 131 suites, all ok), including the tier3 V2/V3/V4/Curve/
  Balancer swap-vs-revmparity suites.
- DB round-trip: liquidity_updater parity + writer tests over the narrowed
  ApplyLiquidityAtTick path (byte-identical TEXT; finding above).
- Solver bench vs the post-KO3SBO recording: solve_curve_path 156 -> 151 us,
  digest_curve_xp 65 -> 60 / 122 -> 116 ns, solve_balancer_stable_path 87 ->
  87 us — flat-to-better (noise band; no lookup-latency regression).
- clippy -p degenbot-bot --all-targets clean; the workspace gate's residual
  allocator_ctrl allow_attributes lints are pre-existing (exposed from
  behind the HTPKLX-era libs; file untouched by this task).

# HTPKLX — consolidated before/after memory report (TZSWOP, 2026-09-04)

## Static sizes (per registered entity)

| Struct | Before | After | Delta |
|---|---|---|---|
| PoolEntry (per registry slot, 18k fleet) | 512 B | 16 B | **-496 B/slot (~-97%)** |
| TickInfo (per initialized tick) | 56 B | 48 B | -8 B/tick (-14%) |
| SimulationResult (built path) | per-hop heap allocs x3 (Vec families) | 1 Box<[StepOutcome]> | -2 allocs + 48 B headers/path |
| SimulatePath + DispatchCandidate | 3 parallel Vecs per candidate | 1 Box<[SolveStep]> | -2 allocs per candidate + headers |
| TickBefore / V3 delta priors | I256 nets (32 B) | i128 (16 B) | -16 B per recorded delta row |

Harness evidence (DEGENBOT_ALLOC_TRACK=1, post-epilogue):
- simulation_result_build_hops2/3/5 = 1 alloc each (128/192/320 B) — was
  3 allocs + headers per family Vec.
- v3_register_128_ticks = 4 allocs / 34465 B; v3_state_clone_128_ticks =
  1 alloc / 16656 B; v2_register_journal8 = 1 alloc; tick_map_build_128 =
  1 alloc — matching the KKNVVS baseline (no per-op regressions).
- Simulation-path merge (4JLQNS/TQBBCO): simulated-path construction
  2 allocs -> 1 at hops=2/3/5; byte-for-byte SolveStep rows replace the
  three parallel Vecs (4JLQNS+TQBBCO commits). The executor's encode path
  keeps its double-slice shape on purpose (grammar_shape plan-time clamp
  re-borrows consumed_inputs; per-submission, not hot).
- PoolEntry boxing (KO3SBO): slot 512 -> 16 B, ~**4.8 MB** of hot map bytes
  dropped at 10k live reserve-pair entries (fleet doc cites 18k pools);
  map probe op-count unchanged (+1 deref behind the map hop); solver
  digest bench flat-to-better (156 -> 151 us solve_curve_path).
- TickInfo narrowing (LIBQKE): 56 -> 48 B/tick; at 50k live initialized
  ticks that is ~400 KB off the tick maps, plus doubled cache density on
  the solver's dominant O(ticks) walk. Note: 8 B above the naive 40 B
  estimate — U128 alignment pads the struct to 16 B multiples.

## Latency (solver digest bench, post-slices vs pre-epic run)
- solve_curve_path: 156 -> 151 us; digest_curve_xp/2coin 65 -> 60 ns,
  /4coin 122 -> 116 ns; solve_balancer_stable_path 87 -> 87 us; CL digest
  vectors 1.04/1.28/1.58 us — all within noise; no accepted regression.

## Consciously NOT taken (with reason)
- Transient per-call structs (IntV3 transient pool rebuild per override
  hop): the harness shows the transient's box is 1 alloc/site, not hot.
- BalancesBlockDelta journal Vecs: touched only on reorg, tracked cost
  unchanged by the epilogue measurements.
- deployments.rs static config: read-once at startup, zero per-op effect.
- Wire-format caching: the executor encode path is per-submission (cold)
  and grammar_shape's plan-time clamp depends on the two-slice shape.
- Keeping V3/V4 PoolEntry pairs inline (article's 'common stays inline'):
  measured slot math still favors boxing — inline V3 width (368 B state +
  identity) exceeds the boxed 16 B slot + one allocation, and uniformity
  keeps the registry layout family-agnostic.

## Broad gates
- cargo check --workspace --all-targets: PASS (0 errors).
- cargo test: full workspace + touched-crate suites — **2774 passed /
  0 failed** across 131 suites (incl. tier3 V2/V3/V4/Curve/Balancer
  swap-vs-revm parity, tick parity, updater/writer DB parity).
- cargo clippy (touched crates per the harness task's convention):
  -p degenbot-bot --all-targets clean; -p degenbot-pools (serde/fmt
  exception expectations updated where the narrowing retired unwraps).
  Pre-existing workspace-only allocator_ctrl allow_attributes lints are
  excluded per the task's convention (untouched file; latent from a
  HEAD gate that never compiled past the earlier failing lib).
- Solvers benches recorded vs baseline (section above).
