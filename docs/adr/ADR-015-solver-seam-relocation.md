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

**Status (2026-07-19): CLOSED — hop stays as `enum ResolvedHop`.**

The deeper review (whether `ResolvedHop` dissolves behind a `trait
PathHopSnapshot { simulate; mobius_shape }`) was evaluated against the
concentrated post-BDZHCG layer and **settled as a negative**: the hop earns
its keep, the enum + match classifier is the right shape, and `dyn
PathHopSnapshot` is a wash-to-loss. The closure is recorded at the end of
this section; the original constraints are retained below as the reasoning
trail so the question is not re-derived.

--- (original findings retained) ---

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

## CLOSURE — the hop-shape deepening (2026-07-19)

Evaluated against the concentrated post-BDZHCG layer. **Verdict: the hop
stays as `enum ResolvedHop` + match-based classifier.** Three negative
findings settle the question end-to-end:

1. **Digest-prework motivation: retired.** The "re-paid ~25× per solve"
   framing (constraint #1 above) was empirically wrong — the digest is
   baked into the hop-state struct **once at resolve**, never recomputed
   per golden-section iteration. The digest bench
   (`rust/crates/degenbot-solvers/benches/digest.rs`, ergo 77LOQT)
   measured Balancer stable `D` at 1.0–1.6 µs and Curve `xp` at 63–114 ns
   vs Phase B solves of 82/144 µs — **digest is 0.04–4% of the per-path
   budget**. The cross-path memoization candidate is empirically rejected
   (CL caches justifiably: its O(N log N) tick walk is 10×+ heavier; the
   light families don't amortize, and a stale-digest reorg bug is worse
   than ~1% wall time). The only surviving reason for the per-solve
   frozen snapshot is constraint #5 (lock-free solve), which is about
   *where the digest lives*, not *what it costs*.

2. **Composition-classifier motivation: settled as a negative (constraint #4).**
   The trait shape (`dyn PathHopSnapshot { simulate; mobius_shape; cl_shape }`)
   eliminates `enum ResolvedHop` but **does not dissolve the work**: the
   9-way `solve_path` composition classifier survives as capability-query
   chains over trait objects, with the same 9 branches and the same
   information. The per-composition search algorithms (`exact_mobius_solve`,
   `int_solve_cl_path`, four golden-section arms, the mixed arm) are
   per-*path-composition* strategies, not per-hop plug-ins — a hop-level
   trait can't replace that dispatch. The one genuine consolidation
   (per-family `simulate_*_hop` free fns → `impl PathHopSnapshot::simulate`)
   is real but small and captured as the cheap inner simulate (sub-µs),
   not the search (82 µs+). Net: same complexity, more machinery (trait +
   dyn dispatch + `Box<dyn>` heap-alloc per hop at resolve vs inline enum
   payload), zero perf gain. Wash-to-loss on depth, clear loss on runtime.

3. **Extensibility motivation: not in favor of the trait.** Adding a DEX
   family under the enum is three local edits (new variant + new solve
   arm + new simulate leaf, classifier extends by one branch). Under the
   trait it's *more* touch points (implement the trait for the new state
   struct + extend the capability queries + new solve arm + extend the
   classifier's `if` chain), because the trait's capability surface has to
   grow alongside the struct. The enum is the lower-friction shape.

### What the hop type still earns

- One aggregate type for the path's hop list (`Vec<ResolvedHop>`, stack-
  allocated, variant payloads inline — no heap per hop, no vtable indirection
  on the 25-iter golden-section simulate loop).
- One classifier site (`solve_path`) where the composition decision lives
  genuinely — not a leak, the decision itself.
- The composition dispatch (closed-form Möbius vs CL closed-form vs four
  golden-section arms vs mixed) is exactly the work a hop-level trait can't
  replace; the enum makes that dispatch explicit and branch-predictable.

### Candidate future improvement (NOT the dyn trait)

The one genuinely good property of the trait shape — collapsing per-family
`simulate_*_hop` free functions behind one method signature — is available
*without* the dyn-dispatch/heap-alloc cost via a private internal trait
`trait HopSimulate { fn simulate(&self, amount_in: U256) -> U256; }`
implemented for each hop-state struct and called via static dispatch.
Even that is cosmetic: the 4 free fns are private to the module and
already share the identical signature shape. Not pursued.

### Resolution of the original session questions

- *"Accept write contention for faster simultaneous reads (RwLock)?"*
  Rejected (constraint #3): pump↔solve are sequential within one task,
  steady-state write contention is near-zero, the `RwLock` exists for
  cross-task writers (construction, snapshot-verify, Python registration),
  not pump-write-vs-solve-read. The framing mis-located the cost.
- *"Memoize digests on state structs across hot pools?"* Rejected (spike
  77LOQT): the digests are light and invalidate per-swap; the cache would
  pay reorg-invalidation correctness risk for ~1% wall-time ceiling.
- *"Remove the hop for structural depth?"* Rejected (this section): the
  classifier survives any trait shape as capability queries; the enum is
  lower-friction for DEX-family extensibility; `dyn PathHopSnapshot` adds
  heap alloc + vtable dispatch for zero perf gain.

The hop-removal research is **closed**. ADR-015 stands as decided
(relocation complete); the deferred deeper review is resolved here, not
re-deferred.

### ArcSwap / synchronization-primitive audit — CLOSED (2026-07-19)

Surfaced from the session's opening question ("accept write contention for
faster simultaneous reads via RwLock?") and the digest-spike's
primitive-cost estimate. Two separate investigations, both closed:

**1. ArcSwap for the digest cache (rejected).** The spike (77LOQT) retired
the digest-cost motivation before the primitive choice mattered: even at
ArcSwap's ~5 ns lock-free read, the light families' digests (1 µs balancer
`D`, 63 ns Curve `xp`) don't amortize against their 82–144 µs solves, and
the stale-read window ArcSwap introduces is a correctness hazard for an
arbitrage-bounding digest that `Mutex`'s block-on-write behavior protects
against in the reorg case. CL is the one family that caches justifiably
(O(N log N) tick walk, 10×+ heavier); `parking_lot::Mutex<TickRangeCache>`
stays. ArcSwap would be the right primitive for a *heavy-digest,
high-read-contention* family that doesn't exist today.

**2. ArcSwap for `Bot.construction_io` (closed — stays as-is).** The slot
(`parking_lot::RwLock<Option<Arc<ConstructionIo>>>`) is the one site in
`degenbot-bot` whose shape (publish-once-at-init, read-many) genuinely fits
`ArcSwapOption`. But the evaluation cascaded: if the slot is truly
write-once, *no* primitive is needed at all — the construction-seam redesign
(make IO a `Bot::new` constructor arg) would drop interior mutability
entirely. That's blocked by `PyBot::new` happening before the provider is
known. Three options (merge seam / `OnceLock` / `ArcSwap`) all have
poor effort-to-value: the slot is uncontended, reads are I/O-dominated,
no profile points at it, no lock-ordering near-miss. Disposition:
**stays-as-is** until a forcing function (runtime IO re-attachment) lands.

**Broader audit.** Three other `parking_lot` sites in `degenbot-bot`
(`Arc<RwLock<BotState>>`, `Arc<Mutex<ArbitrageEngine>>`,
`Mutex<HashMap<…subscribers…>>`) are **incrementally-mutated state**, not
publish-snapshots — the wrong model for ArcSwap (would require COW-cloning
whole state per mutation). No candidate fits. `arc-swap 1.9.2` stays
transitive-only in `Cargo.lock`; no `degenbot-*` crate pulls it directly.

Recorded in `CONTEXT.md` ("Synchronization primitive for
`construction_io`") so these threads aren't re-litigated without a
forcing function.
