# ADR-015: Solver-Seam Relocation — the Resolve→Solve Boundary

**Status: accepted (decision).** The relocation itself is a candidate slice
(tracked separately under ergo; see CONSEQUENCES). This ADR records the
*architectural* decision so future architecture reviews do not re-suggest
leaving the pure solve layer co-located with the I/O orchestrator in
`degenbot-bot`, and so a planned deeper review of the hop's shape resumes
with this session's findings rather than re-deriving them.

## Context

The arbitrage solver layer is the most-recently-changed code in the repo
(QuantAMM basket solver, Balancer weighted/stable + Curve solve branches
landing in `ArbitrageEngine` across 2026-06/07). It is split across two
crates by an accidental line:

- `degenbot-solvers` — created explicitly for "value-only multi-hop Möbius
  solver math … no chain / registry / async / tokio … consumable by both
  the standalone Rust path (`cargo add degenbot-solvers`) and the PyO3
  driver shell." It holds the V2 + CL Möbius solvers (`mobius_int`,
  `mobius_int_exact`, `mobius_v3_int`, `affected_keys`).
- `degenbot-bot/src/solvers/arb_engine/` — the I/O orchestrator (tokio,
  `core: Arc<RwLock<BotState>>`, path registry, V3/V4 event buffers,
  rayon dispatch). It ALSO holds the pure solve family that never crossed
  the seam:
  - `solve_path` (the dispatcher) + `solve_balancer_weighted_path_int` /
    `solve_balancer_stable_path_int` / `solve_curve_path_int` /
    `solve_solidly_path_int`, all receiver-free
    (`#[allow(clippy::unused_self)]` associated fns on `ArbitrageEngine`);
  - the `simulate_*_hop` swap leaves the golden-section search calls;
  - `balancer_weighted_basket.rs` (the QuantAMM closed-form solver — its
    own header says "NOT a solve_path arm … feature parity with Python's
    `BalancerMultiTokenSolver`", and it has zero `BotState`/tokio/Arc
    imports);
  - the hop-state value types: `ResolvedHop`, `ResolvedMixedPath`,
    `SolvePathResult`, `SolidlyHopState`, `BalancerWeightedHopState`,
    `BalancerStableHopState`, `CurveStableswapHopState` (all in
    `arb_engine/mod.rs`, next to `core: Arc<RwLock<BotState>>`).

The `solve_path` dispatcher already crosses the seam for the V2/CL arms
(`::degenbot_solvers::mobius_int_exact::exact_mobius_solve`) but reverts to
`Self::solve_balancer_weighted_path_int` for the Balancer arm. A standalone
`cargo add degenbot` consumer gets the Möbius solvers but NOT Balancer /
Curve / Solidly / basket — they are locked inside the orchestrator crate
that drags in tokio + BotState + pyo3-wrapper access. This violates the
standalone-Rust-core directive in AGENTS.md ("anything a standalone Rust
consumer would need … must live in a core crate from day one — never
'move it later,' which strands it across the future crate boundary") and
the `degenbot-solvers` crate's own stated mission.

An architecture review (`/improve-codebase-architecture`, 2026-07-19)
surfaced this as the top deepening opportunity. A grilling session then
tested the deeper question — whether the hop type itself needs to exist
at all — and established the constraints recorded below in DEFERRED. The
relocation is robust to that deeper question: moving the hop types + dispatch
into `degenbot-solvers` is valuable whether the hop eventually dissolves
behind a trait or stays a named enum, because it concentrates the whole
pure solve layer in one place so the deeper question gets answered against
the full layer at once, not piecemeal across two crates.

## Decision

### D1 — Complete the seam. The pure solve layer moves to `degenbot-solvers`.

Move, from `degenbot-bot/src/solvers/` to `degenbot-solvers`:

- `solve_path` (the composition dispatcher) + every `solve_*_path_int`
  receiver-free arm (Balancer weighted/stable, Curve, Solidly) + the
  `simulate_*_hop` swap leaves;
- `balancer_weighted_basket.rs` (the QuantAMM basket solver);
- the hop-state value types and their boundary value types:
  `ResolvedHop`, `ResolvedMixedPath`, `SolvePathResult`,
  `SolidlyHopState`, `BalancerWeightedHopState`, `BalancerStableHopState`,
  `CurveStableswapHopState`, `HopType`, `MixedPoolRef`, `PoolHop`,
  `SOLIDLY_GOLDEN_SECTION_ITERATIONS`, `INT128_MAX`.

`degenbot-bot` keeps `resolve_path` (the only core-bound step — it reads
`BotState` under `core.read()` and projects into the solver's intake types).
The orchestrator's solve-side import collapses to
`degenbot_solvers::solve_path(&resolved)`, the call it already makes for the
V2/CL arms.

### D2 — The hop types are the solver's intake contract. They live with the solver.

The hop-state value types are `degenbot-solvers`'s intake protocol, not
pool vocabulary and not math-leaf vocabulary. A hop is **the solver's
snapshot-and-classifier adapter** (see the hop-shape findings in DEFERRED):
it captures pool state at resolve time so the solve can run lock-free under
rayon `par_iter`, and its enum variants let `solve_path` pattern-match on
path composition to choose the algorithm (Möbius closed-form for all-V2 /
all-CL; golden-section for paths involving non-Möbius leaves). The pool
state structs do not know about hops; the math leaves do not take hops as
args. The hop exists for the solver. Its home is with the solver.

This placement is consistent with the CL sibling once the projection move
(ADR-014 / Candidate 2) is accounted for: `IntV3TickRangeHop` lives in
`degenbot-pools` today *only because* `v3_state.rs` / `v4_state.rs` build it
themselves (Candidate 2 already half-done for CL) and the type followed its
builder. That is a side effect of the projection location, not a principle
for where hop types live generally. Under D2 the non-CL hop types relocate
to `degenbot-solvers` as that crate's intake contract; the CL hop type's
eventual home is a function of the DEFERRED deeper review, not this ADR.

### D3 — `resolve_path` stays core-bound in `degenbot-bot`. Boundary at the resolve→solve line.

The resolve→solve boundary is the seam. `resolve_path` reads `BotState`
under `core.read()` and projects into `degenbot_solvers::{ResolvedHop, …}`;
the guard drops; `solve_path(&resolved)` runs lock-free in `par_iter`. The
lock-drop discipline (ADR-005 slice 15b-1: "the guard drops before
`solve_path` runs") is preserved exactly — only the *location* of the pure
functions and the *owner* of the intake types change. The lock-free solve
invariant (recorded below) is load-bearing for the DEFERRED review and
must survive the relocation unchanged.

### D4 — Dependency graph stays a DAG; no-pyo3 invariant preserved.

`degenbot-solvers` already depends on `degenbot-v2-math`, `degenbot-cl-math`,
`degenbot-pools` (and transitively, via `pools`, on all the math leaves).
To absorb the new solve arms + hop types it adds direct deps on
`degenbot-balancer-math` (for `PowVersion` + `weighted_math` leaf),
`degenbot-curve-math` (for `stableswap` leaves + `YVariant`/`DVariant`),
`degenbot-solidly-math` (for `calc_exact_in_stable_solidly`), and
`degenbot-uniswap` (for `DexVariant`). All are leaf math crates already
depended on by `degenbot-pools`, so no new reachability edge appears; the
graph stays a DAG (nothing depends back on `degenbot-solvers` except
`degenbot-bot` and the workspace root). All target crates are core/pyo3-free
(`just check-no-pyo3-in-cores` survives). `rayon` stays in `degenbot-bot`
— the `par_iter` is orchestration over the resolved set; the pure solve
fns do not call `par_iter`.

## Consequences

- The standalone Rust consumer (`cargo add degenbot`) gets the whole
  solver layer — Balancer / Curve / Solidly / basket — not just V2/CL. The
  arbitrary asymmetry (V2/CL standalone, Balancer/Curve/Solidly locked in
  the orchestrator) is removed.
- `solver_dispatch.rs` (1 719 lines) splits along the resolve→solve seam:
  `path_resolution` stays in `degenbot-bot`; `solver_dispatch` (pure)
  relocates to `degenbot-solvers`. The `#[allow(clippy::unused_self)]`
  annotations disappear — receiver-free functions stop pretending to be
  methods.
- `degenbot-bot`'s solve-side import is `degenbot_solvers::solve_path`;
  `degenbot-bot/Cargo.toml` keeps its `degenbot-solvers` dependency (already
  present). `degenbot-python/src/c_api.rs` PyO3 wrappers for the basket
  solver update their call path from `degenbot_bot::solvers::…` to
  `degenbot_solvers::…`.
- `ArbitrageEngine`'s `#[allow(clippy::unused_self)]` methods become free
  functions / inherent fns on the solver module. The engine struct keeps
  only the genuinely-coupled `&self` state (path registry, buffers, core
  handle, rayon orchestration).
- Tests that exercise `solve_path` against hop-state fixtures (incl. the
  QuantAMM basket parity test and the Curve/Balancer swap-leaf parity
  benches) relocate to `degenbot-solvers/tests`. Tests that exercise
  `resolve_path` / the engine lifecycle / event routing stay in
  `degenbot-bot`.
- The relocation is a pure refactor — no behavior change. Red/Green TDD:
  tests stay green throughout; the move is staged (re-export from old
  location to keep callers compiling, update call sites, drop re-exports).
- Does NOT touch `resolve_path`'s projection logic (Candidate 2), which stays
  inlined in `degenbot-bot` for this slice. The projection deepening is the
  job of a follow-up, gated on the DEFERRED hop-shape review (see D2's note
  on the CL hop type's eventual home).

## DEFERRED — the hop-shape deepening (constraints for the resuming session)

The grilling session that produced this ADR tested whether the hop type
needs to exist at all, and whether the solver could instead read live pool
state behind an `RwLock`. The deeper review (whether `ResolvedHop`
dissolves behind a `trait PathHopSnapshot { simulate; mobius_shape }`) is
**deferred to a separate session**. It resumes with these established
constraints, so a future review does not re-derive them:

1. **The hop's snapshot role is selectivity, not copy-avoidance.** The CL
   hop is not "the V3 state, cloned" — it is built by `v3_state.rs` itself
   (`build_int_v3_sequence` → `compute_tick_ranges`) by walking the tick
   map once, in the swap direction, up to `max_ranges + 10` initialized
   ticks, capping at ≤15 ranges regardless of how many thousands of
   initialized ticks the pool has. Each `IntV3TickRangeHop` carries 7
   scalars and a pre-accumulated `liquidity`; `sqrt_price_lower/upper` are
   recomputed from the tick index via `TickMath` once at projection time,
   not read from `tick_data` per solve iteration. The same projection
   pattern holds for the balance-vector family: `BalancerStableHopState`
   carries a pre-computed invariant `D` (one `calculate_invariant` Newton
   run under the lock, not ~25× during the golden-section search) and
   BPT-skips the balances; `CurveStableswapHopState` carries the
   rate-adjusted `xp` array. **Live-read over the pool state would re-pay
   the projection ~25× per solve** (golden-section) or relocate it as
   cache-on-state with invalidation spread across every `apply_*` arm.
   Either is strictly worse than project-once-read-many on an immutable
   snapshot.

   > **Empirically revised (2026-07-19 spike, ergo 77LOQT):** the
   > "~25× per solve" framing above was *wrong* — `BalancerStableHopState`.
   > `invariant` and `CurveStableswapHopState.xp` are baked in **once at
   > resolve**, never recomputed per golden-section iteration (the solve
   > reads the frozen hop-state field). The digest is paid once per *path*
   > per resolve, not once per golden-section probe. Measured
   > (`rust/crates/degenbot-solvers/benches/digest.rs`, criterion): Balancer
   > stable `D` = 1.0–1.6 µs/pool, Curve `xp` = 63–114 ns/pool; vs Phase B
   > solve = 82 µs (balancer) / 144 µs (curve) per 2-hop path. The digest is
   > **0.04–4% of the per-path budget** — solve dominates entirely. CL is
   > the one family that caches, and justifiably: its `compute_tick_ranges`
   > is an O(N log N) scan over thousands of ticks + ~30 TickMath calls —
   > an order of magnitude+ heavier than balancer's `D` (the existence of
   > `cached_tick_ranges` is the proof it pays). The candidate-family
   > "extend CL's memoization" optimization is **empirically rejected**:
   > the ~1% wall-time ceiling on hot pools does not justify the reorg-
   > invalidation correctness risk (stale digest → wrong arb, silently) for
   > balancer, and Curve's 63 ns is dwarfed by the `Mutex`+`Arc` overhead
   > caching would add. The digest-cost argument is now **off the table**
   > as a motivation for any hop-shape change; constraint #5 (lock-free
   > solve: guard drops before `solve_path`) is the only surviving reason
   > for the per-solve frozen snapshot, and it is about *where the digest
   > lives* (shared mutable state behind `RwLock`), not *what it costs*.

2. **Consistency across hops in a multi-hop path requires an immutable
   snapshot or a read-guard held across the whole multi-hop solve.** Hop N
   at block B and hop N+1 at block B+1 is an inconsistent path → wrong
   optimal input → wrong arbitrage. Today this is free (immutable snapshot,
   resolved under one guard then dropped). A held-guard-for-the-whole-solve
   variant blocks the rare-but-real concurrent writers (Python-driven
   `register_pool`, snapshot-verify, construction) for the duration of the
   search across all affected paths — even though pump↔solve are sequential
   within a block (see (3)).

3. **Pump and solve are sequential within one task; steady-state
   write-contention is near-zero.** `run_with_stream` drives
   `process_block` = `apply all logs (core.write()) → solve_dirty
   (resolve under core.read()→drop, then par_iter solve)`. There is no
   `tokio::spawn` of solve concurrent with the pump. The `RwLock` on
   `core` exists for cross-task writers (construction, snapshot-verify,
   Python-driven registration), not pump-write-vs-solve-read. So the
   "accept write contention for faster simultaneous reads" framing
   mis-locates the cost: the snapshot is not paying a memory penalty to
   avoid contention — it is the mechanism that makes the search *fast*
   (project-once-read-25×-cheap) *and* *consistent* (immutable across a
   multi-hop path) *and* *lock-free* (drops the guard so concurrent
   writers aren't blocked), all at once, for the cost of cloning the
   already-computed projection (tens-to-low-hundreds of bytes per hop).

4. **C-prime (the `dyn PathHopSnapshot` trait shape) sacrifices
   `solve_path`'s composition classifier.** `solve_path` pattern-matches
   on the *composition* of the path (`all_v2`? `all_cl`? `has_solidly`?)
   to pick the algorithm (closed-form Möbius vs golden-section vs mixed).
   If every hop becomes `dyn PathHopSnapshot`, the classifier must survive
   as a capability query ("are all hops Möbius-shaped?"), which is a thinner
   hop re-invented behind a trait. C-prime also forces Candidate 2's
   projection to ship *with* it (the projection must build the minimal
   snapshot), landing three candidates' worth of decisions at once.

5. **The current lock-free solve invariant must survive any hop-shape
   change.** "The guard drops before `solve_path` runs" (ADR-005 slice
   15b-1) is the load-bearing line; an interface that re-couples solve to
   the core lock regresses on the discipline the codebase explicitly chose.

## Why not the alternatives

- **Leave the pure solve family in `degenbot-bot` (status quo).** Rejected:
  violates the standalone-Rust-core directive and the `degenbot-solvers`
  crate's own stated mission; a standalone consumer cannot dispatch a mixed
  Balancer path without re-implementing `solve_path`; pure math is
  co-located with I/O machinery (locality loss).
- **A new `degenbot-arbitrage` crate instead of reusing `degenbot-solvers`.**
  Rejected (YAGNI): the home exists and advertises this exact mission. Its
  description ("V2 constant-product + V3 concentrated-liquidity") widens to
  the full family; that's a doc update, not a new crate.
- **Hop types to `degenbot-pools` (next to the state structs).** Rejected as
  *this* slice's decision: see D2. The hop is the solver's intake contract,
  not pool vocabulary. (Revisitable as part of DEFERRED — specifically for
  the CL hop type, whose `pools` placement today is a side effect of the
  projection location, not a principle.)
- **Dissolve the hop entirely (C-prime) before relocating.** Rejected as
  premature under grilling: lands three candidates' worth of decisions at
  once (relocation + trait interface + projection-on-state-struct), and
  re-invents a thinner hop behind a trait (selectivity + classifier
  survive). Relocate first; attempt C-prime against the whole layer in one
  place, later.
- **Hold the core read-guard across the solve (read-live-state variant).**
  Rejected (constraints 1–3 above): re-pays the projection ~25× or relocates
  it as cache-on-state with cross-`apply_*` invalidation; blocks concurrent
  writers for the solve duration; the snapshot's clone is the projected
  minimal form (tens of bytes), not the pool state.
