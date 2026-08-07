# Plan: Deepen `bot_core` — split the `BotState` god-file into structural-family orchestration modules

Status: **DECIDED** via an architecture review (improve-codebase-architecture + grilling loop, 2026-06-10).
Owner: Rust core (`degenbot-bot/src/bot_core/`). No Python surface changes.

## Background (read first)

`rust/crates/degenbot-bot/src/bot_core/mod.rs` is an **8,504-line god-file**. It holds:

- the `BotState` struct (lines 122–~320) — the ADR-003 **single state owner**;
- a ~3,700-line `impl BotState` (lines ~594–4364) containing the orchestration methods for
  **five** pool family blocks (V2-, V3-, V4-, Curve-, Balancer-weighted/stable),
  the CL-common dual-buffer / snapshot / quarantine / coverage methods,
  the reorg dispatch, and the registry / solver-facing calc;
- one `mod tests` (~4,100 lines, lines 4399–8504) covering all five families.

The structural-deepening opportunity: ADR-003 made `BotState` the single state owner — the right
call for **leverage** (one deep module behind a small handle). But the *module* is one file, so a
developer answering "what does registering a V4 pool do?" must wade a file that owns all five
families. The family **state structs already live in sibling files** (`v3_state.rs`,
`curve_state.rs`, `balancer_*_state.rs`; V2/V4/FoT state types in the separate `degenbot-pools`
crate), and the `impl BotState`-from-a-sibling-module pattern is **already proven** here:
`bot_core/divergence_probe.rs` adds an `impl BotState { … }`, and child modules of `bot_core` may
reach `BotState`'s private fields. ADR-016 already sliced the reorg dispatch out of this same file
for exactly this reason.

The deepening is **impl-block extraction**, not logic change. The interface (`BotState`) is
unchanged; only the module organization shifts so per-family *behavior* sits beside per-family
*type* and per-family *tests*.

## Domain vocabulary (from CONTEXT.md — load-bearing)

The split groups by CONTEXT's **three structural families**, NOT by DEX:

- **Reserve-pair** — V2, AerodromeV2 (share `V2BlockDelta` / full-state sync overwrite).
- **Balance-vector** — Curve, Balancer-weighted, Balancer-stable (share the byte-identical
  `BalancesBlockDelta`).
- **Concentrated-liquidity (CL)** — V3, V4 (structurally near-identical; `slot0` scalars +
  `tick_data`; partial-prior `V3BlockDelta`; unified behind the `ConcentratedLiquidityPool(Mut)`
  trait already adopted for the CL family only).

New module names reuse this exact vocabulary:
`cl_orchestration.rs` (this pilot), then `reserve_pair_orchestration.rs`,
`balance_vector_orchestration.rs`. No CONTEXT.md glossary additions are required.

## Decisions (all settled in the review — do not re-litigate)

**Q1 — Split boundary: impl-methods only; the `BotState` struct stays central.**
The struct is already a clean single registry (`pools`, `pool_addresses`, `tokens`,
`next_pool_id`, `journal_depth`, v3/v4 buffers, snapshot seed, pump cutoff). Splitting its fields
into per-family sub-structures would break the cross-family methods that need one registry + one id
space (`pool_id_by_address`, `unregister`, `apply_swap_by_pool_id`, `get_v3_or_v4_pool`). Only the
*impl method blocks* move.

**Q2 — Module home: a new sibling file per structural family in `bot_core`.**
Consistent home for each family's `BotState` methods. Do NOT fold orchestration into the
`degenbot-pools` state crates — that would blur the I/O-free seam (ADR-001) and be inconsistent
(V2/V4 have no local file there). `bot_core/<family>_orchestration.rs` keeps the crate boundary crisp.

**Q3 — Rollout: pilot the CL slice first, land green, then generalize.**
CL is the largest block with the most recent churn and is impossible to cut one-DEX-at-a-time
(V3/V4 share the CL-common methods), so it is both the highest-value and the natural pilot unit.

**Q4 — Tests: co-locate per-family tests with the family module.**
The single ~4,100-line `mod tests` splits; each family's test blocks move beside their impl.
Cross-family tests (`get_v3_or_v4_pool_*`, reorg, registry) stay resident in `mod.rs`'s test module.

**Q5 — Group by structural family** (see vocabulary above), not six per-DEX files.

**Q6 — The pilot is the CL slice as a unit** (V3 + V4 + the shared CL-common
dual-buffer/snapshot/quarantine/coverage methods), not "just V4".

**Q7 — CL-common accessors go INSIDE `cl_orchestration.rs`** with the other CL state methods.
They are state accessors, not choreography; the ADR-022 *sequence* (quarantine→drain→verify→Live)
lives in the untouched `registration_lifecycle.rs` and calls them by name.

**Q8 — No external callers yet (closed development), so internal organization is free.**
Keep `mod.rs` as the assembly + `pub use` re-export hub so the `degenbot` umbrella, the PyO3
binding, and all tests remain call-site-neutral. Treat this as a strict organizational refactor.

**Q9 — Cross-family CL-*consuming* solver calc stays resident in `mod.rs` for the pilot.**
`get_v3_or_v4_pool`, `simulate_exact_input/output_swap_*`, `calculate_tokens_out_with_fetch`,
`simulate_swap_with_override`, `process_backfill_logs`, `calculate_tokens_in(s/out)` — these read
V3/V4 state but are the solve-facing layer spanning all families. They move later (if ever) with a
solver-facing carve-out, not the family slice.

**Q11 — Wiring: `pub use` aliases keep the public surface stable.**
Each moved module carries its own `use` block and reaches `BotState` private fields directly
(precedent: `divergence_probe.rs`). `mod.rs` re-exports the moved methods via
`pub use cl_orchestration::{register_v4_pool, …}` so internal callers are untouched.

## The CL pilot cut list (Q10)

**Move into `bot_core/cl_orchestration.rs` (pure relocation + per-module `use`):**

- V3 family: `register_v3_pool`, `update_v3_pool`, `apply_v3_swap`, `apply_v3_swap_by_pool_id`,
  `apply_v3_liquidity_update`, `apply_v3_liquidity_update_by_pool_id`, `sync_tick_data_by_pool_id`,
  the V3 journal methods, `sync_v3_pool_state`, `merge_tick_word`, `get_v3_pool`, `get_v3_identity`,
  `v3_pools_snapshot`.
- V4 family: `register_v4_state_view`, `state_view_for`, `register_v4_pool`, `apply_v4_swap`,
  `apply_v4_swap_by_pool_id`, `apply_v4_liquidity_update`, `apply_v4_liquidity_update_by_pool_id`,
  `get_v4_pool`, `get_v4_identity`, `v4_pool_id_by_key`.
- CL-common dual-buffer (V3+V4 twins together): `apply_backfill_buffer_v3/v4`,
  `apply_pump_buffer_v3/v4`, `buffered_v3/v4_event_count`, `flush_v3/v4_buffer`,
  `expire_v3/v4_buffered`, `apply_buffered_v3/v4_event`, `buffer_backfill_v3/v4_liquidity_update`,
  `set_v4_buffer_max_age`.
- CL-common snapshot: `set_snapshot_seed_block`, `v3/v4_snapshot_seed`, `take_v3/v4_snapshot_seed`,
  `pin_v3_post_drain_snapshot`, `take_v3/v4_post_drain_snapshot`.
- CL-common coverage/quarantine/lifecycle accessors: `v3/v4_pool_coverage`,
  `set_v3/v4_pool_quarantined`, `set_v3/v4_pool_live`, `release_all_v3_v4_quarantined`.
- `get_v3_or_v4_pool` (the CL-family read), **plus their co-located test blocks.**

**Stays resident in `mod.rs` (for now):**

- Cross-family dispatch: `apply_swap_by_pool_id`, `apply_liquidity_update_by_pool_id`,
  `seed_genesis_by_pool_id`.
- Registry / reorg: `pool_id_by_address`, `unregister_pool`, `pool_count`, `pool_family`,
  `has_pool`, `has_token`, `token_entry`, `restore_all_pools_before_block`,
  `restore_pool_before_block`, `discard_pool_before_block`, `pool_journal_len`,
  and the ADR-016 unified reorg dispatch.
- Solver-facing calc: `calculate_tokens_*`, the `simulate_*` family, `simulate_swap_with_override`,
  `encode_swap`, `process_backfill_logs`.
- The full reserve-pair (V2 + Aerodrome) and balance-vector (Curve + Balancer) blocks and their tests.

## Risk mitigations

- **Pure relocation — RED-neutral→GREEN.** No logic is edited, renamed, or reordered across a
  behavioral seam. Any behavioral diff (a test that starts failing, or a test that would need
  changing to pass) is treated as a **split bug**, not a feature. The pilot gate is
  `just test-rust` + `just lint-rust` + `just check-no-pyo3-in-cores` fully green with **zero**
  test-body edits beyond the mechanical test-block move.
- **Call-site neutrality.** Because `mod.rs` re-exports the moved methods, the `degenbot` umbrella
  (`rust/crates/degenbot/src/lib.rs`), the PyO3 binding, the Tier-0 `standalone_consumer.rs`, and
  every test keep compiling unchanged. This is what lets one commit land a pure move.
- **Fields stay private and centralized** — do not loosen field visibility to satisfy a moved
  method. Moved `impl BotState` blocks reach private fields legitimately as child modules; if a
  carve-out method cannot reach a field cleanly, that is a signal the carve-out boundary is wrong
  (it belongs in the family slice or in the resident dispatch), not a reason to make fields `pub`.
- **`module inception` / `missing_docs` / clippy** must stay clean on the new modules (project
  lints are strict). Add one-line doc comments at each new module + re-export.
- **No `pub` surface growth.** If a moved method needs to become `pub` for `mod.rs` to re-export it
  (it already will be, as `impl BotState` methods are `pub`), that is expected; but do not add NEW
  public methods or change signatures — `bot_core::*` reaches the same set of names before and after.

## Clean execution ideas (ordered)

1. **D1 — Inventory + classification is its own task.** Before any move, enumerate every method in
   the `impl BotState` and classify it (CL / reserve-pair / balance-vector / cross-family-dispatch /
   solver-calc) with an explicit decision for each. This is the load-bearing artifact the later move
   tasks consume; it prevents a "move it because it's near the V4 block" error during the cut.
   Land the classification table in the task body / result. (Fresh inventory beats stale line numbers
   from this plan — line ranges drift.)
2. **D2 — Create the CL module skeleton + re-export aliases, then move the CL slice.**
   One module, one change-set per family slice. Move impls + port `use` blocks; do NOT move tests in
   this change (keeps the move reviewable); add `pub use cl_orchestration::{…}` to `mod.rs`.
3. **D3 — Co-locate the CL test blocks** into `cl_orchestration.rs`'s `mod tests` (move, not copy),
   leaving cross-family tests resident. This is a second, mechanical change.
4. **D4 — Pilot gate.** Full green (see risk mitigations). Confirm call-site neutrality by compiling
   `standalone_consumer.rs` + the PyO3 binding untouched.
5. **D5 — Generalize** to `reserve_pair_orchestration.rs` (V2 + AerodromeV2) then
   `balance_vector_orchestration.rs` (Curve + Balancer×2), each as a D2+D3 pair, landing green each time.
6. **D6 — Bookkeeping.** Record the decision (this plan) + a short ADR capturing the
   structural-family grouping and the `*_orchestration.rs` home so a future review that sees
   `mod.rs` shrink does not misread it as consolidation-by-happenstance or re-suggest the old
   monolith. Update `CONTEXT.md` "Resolve→solve boundary" / "Pool structural families" links if the
   file layout changes materially. `docs/migration-guides/` note optional.

## Non-goals (explicit)

- **Do NOT split the `BotState` struct** (ADR-003 single registry is load-bearing).
- **Do NOT move V2/V4 state types into `degenbot-pools` orchestration** — the crate is I/O-free
  state structs (ADR-001); orchestration stays in `bot_core`.
- **Do NOT touch `registration_lifecycle.rs` / `snapshot_verify.rs` / `liquidity_verifier.rs`** —
  the ADR-022 verify sequence and the verifier leafs are already-correct homes; only the
  coverage/quarantine *state accessors* move, and they move into the CL slice.
- **Do NOT re-home the solver-facing calc / CL-*consuming* simulate methods in this epic** — defer
  to a solver-facing carve-out.
- **Do NOT change public `bot_core::*` names or any behavior.**
- **No new dependencies, no feature gates, no Python changes.**

## ADR touchpoints

- **ADR-003** (Bot as single state owner) — `BotState` stays the single owner; this only reorganizes
  its implementation home. The struct + identity/reorg dispatch remain the central registry.
- **ADR-001** (I/O-free pools) — respected: no orchestration enters `degenbot-pools`.
- **ADR-005** (three-layer FFI) — unaffected: this is a `bot_core`-internal refactor; the PyO3 and
  Python surfaces are untouched.
- **ADR-014 / ADR-016** — precedent for the slice-into-sibling-modules pattern; the reorg dispatch
  already split out this way. This continues that trajectory for the family orchestration.
- **ADR-022** (registration verify-lifecycle core-owned) — untouched; `cl_orchestration.rs` holds only
  state accessors the lifecycle calls.
