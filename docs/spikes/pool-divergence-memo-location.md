# Spike: where the divergent-pool memo lives

## Decision

**Option (b): Rust-core.** The divergent-pool memo lives on
`ArbitrageEngine` (alongside `pool_to_paths`), with the per-block
aggregation + the path-skip running inside `dispatch_profitable` at
`rust/crates/degenbot-backrun-strategy/src/dispatch.rs` — the same leaf
the thin-margin pre-filter already occupies.

Option (a) (Python-side `EngineRegistry`-held `set[Address]`) is
rejected.

## Reasoning

The decision is settled by precedent, not a judgment call. The thin-margin
pre-filter — the exact analog of "operator-cockpit path-skipping concern
over `DispatchCandidate`s" — already lives in the Rust core at
`rust/crates/degenbot-backrun-strategy/src/dispatch.rs:103`
(`filter_thin_margin_results`). Its §4.2 note states the principle
verbatim:

> the leaf is standalone-usable (pure int + zero allocation beyond the
> kept vec); it is defined here so a standalone Rust consumer
> (`cargo add degenbot`) doesn't need Python for the pre-filter. It may
> later move to a shared crate if a second consumer emerges.

That IS the ADR-005 standalone-Rust-consumer constraint applied to this
class of concern. Path-skipping over `DispatchCandidate`s based on
per-pool solver-divergence is structurally identical to path-skipping over
`DispatchCandidate`s based on thin-margin: both are pure decisions over
the candidate list the dispatch already holds, both matter identically to
a Rust consumer + a Python driver, neither needs Python-ecosystem
coupling (no `Fraction`, no I/O, no ORM).

Option (a) would strand the standalone Rust consumer without the
divergence signal — the literal anti-pattern ADR-005 §4.2 warns against.
Because the failure inputs (`SimFailure.captured_swaps[].emitter` +
`hop_outputs` + `bucket`) already flow into the dispatch outcome, the
feedback loop (dispatch result → divergent-pool memo → next-dispatch
skip) closes entirely in Rust-core state, no Python round-trip per block.

## Implementing shape (for task GMWYIU)

- `ArbitrageEngine` gains a field (alongside `pool_to_paths` at
  `rust/crates/degenbot-bot/src/solvers/arb_engine/mod.rs:332`):
  `divergent_pools: HashMap<(HopType, u64), u64>` — pool_key →
  consecutive-block SolverCalc count (the "recently divergent" signal).
  Decays/clears on a block window (TBD: simple N-block expiry vs. a
  rolling count; spike recommends N-block expiry for v1 — simplest
  semantics, mirrors the path-suppress retry interval).
- Mutation path: `dispatch_profitable` (or its outcome consumer in the
  engine) calls a new `engine.record_pool_divergence(block, failures)`
  after each batch, bumping per-pool counts where `classify_candidate`
  returns SolverCalc (same logic the Python aggregator from task BEGMB5
  uses — factored into a shared leaf, not duplicated).
- Skip path: `DispatchCandidate` construction (or the thin-margin
  filter's sibling in `dispatch.rs`) calls
  `engine.is_path_divergent(path_id)` (a read over `pool_to_paths` ∩
  `divergent_pools`); divergent paths are dropped pre-sim, counted in a
  new `divergent_dropped` outcome field (mirroring `thin_dropped`).
- The Python `[pool-divergence]` render from task BEGMB5 stays — it now
  reads the Rust-side memo via a new `outcome.divergent_pools()` getter
  (single source of truth), rather than re-aggregating from
  `outcome.failures()` Python-side. The Python aggregator function
  `aggregate_pool_divergence` is retired (its logic moves to the shared
  Rust leaf) — no backwards-compat layer.

## Risk

The feedback loop (dispatch → memo → next-dispatch skip) lives in
engine state mutated from the dispatch path. Lock ordering: the engine
is `Arc<RwLock<BotState>>` (ADR-003); the divergent-pools field is on
`ArbitrageEngine`, not `BotState`, so it's under the engine's own lock
(no nested locking with `BotState`). Confirm with the existing
`ArbitrageEngine` lock-discipline tests when implementing.
