# ADR-060: Collapsing the V2 walk dispatch — representation vs interface

**Status: accepted** (2026-09-23). Basis: the affected-site survey of
`rust/crates/degenbot-solvers` recorded in Context below, and the
rounding-parity analysis of `IntHopState::swap` against
`compute_swap_step_v3`. The deciding empirical evidence (a byte-parity
probe over captured paths) is a later task; this ADR fixes the decision
criteria that probe must meet and picks a provisional direction.
Predecessors: ADR-014 (pool-state deepening), ADR-015 (resolve→solve
seam), ADR-038 (CL event routing), ADR-058 (strategy crate split),
ADR-059 (pool family kernel, whose D6 "hot loops stay monomorphic"
discipline constrains this change).

## Context

The active-set piecewise Möbius walk in `src/cl/active_set.rs` is the
single solve path for CL and mixed V2+CL arbitrage paths. Its hop type
is a crate-private sum:

```rust
pub(super) enum WalkHop<'a> {
    ConstantProduct(&'a IntHopState),
    Cl { crossings: Arc<ClCrossingTable>, profiles: Arc<ClProfileTable> },
}
```

The walk is otherwise family-agnostic: it discovers pieces by simulating
landed ending-range tuples, projects each piece into Möbius coefficients,
and refines around the argmax. Two places still branch on the enum:

1. `simulate_walk_path_inner` (the forward, self-determined-crossing
   simulation), and
2. `walk_event_first_above_predicted_inner` (the exact-out inversion that
   predicts the first input whose landed tuple exceeds `ks`).

Everything else that touches the hop either already treats both arms as
one parametric piece (`build_shifted_piece_hops`), or reads a per-hop
capability that the enum encodes (`iteration_cap`, `single_piece_path`,
`saturation_edge`, telemetry range counts).

The epic goal is to collapse that dispatch into the unified piecewise
Möbius walk. Two shapes are available:

- **A — representation collapse.** Delete `WalkHop::ConstantProduct` and
  project every V2 hop as a degenerate single-range CL tick-range
  sequence, so the existing CL step simulators run V2 too.
- **B — interface collapse.** Replace the enum with a parametric per-hop
  piece view that carries a family-specific step simulator; keep the V2
  and CL step math separate, but delete the enum match sites.

The choice is decided by rounding parity, not by taste. The walk's
correctness is anchored on the landed ending-range tuple, and a
single-wei difference in a hop's output can move a candidate across a
`crossing_gross_input` boundary, changing `landed`, the visited piece
sequence, and the recorded profit.

### The rounding-parity crux (verified)

**V2.** `IntHopState::swap` (`degenbot-math/src/v2/hop_state.rs`) is a
single EVM floor on one rational:

```
out = (x·γ·R_out) / (R_in·fee_denom + x·γ)   // one DIV, no ceiling
```

**CL.** `simulate_v3_range_swap` (`src/cl/hop_sim.rs`) walks the range
entry → interior word boundaries → exit, one `compute_swap_step_v3`
(`degenbot-math/src/cl/swap_math.rs`) per target. The exact-in step
computes `amount_in = ceil(Δ_in)` (`get_amount_delta(..., round_up=true)`),
`amount_out = floor(Δ_out)`, and `fee_amount = ceil(amount_in · fee_pips /
(MAX_SWAP_FEE − fee_pips))`; the gross consumed is `amount_in + fee_amount`
and accumulates per step. Dense ranges route through `ClWordProfile::swap`,
documented byte-equivalent to that walk.

The CL projection of a V2 hop is lossy at both ends: `IntV3TickRangeHop`
is derived from an actual pool's `L`/`√P`/bounds, and
`to_int_hop_state` (`degenbot-pools/src/int_v3_hop.rs`) truncates the
virtual reserves `L·2^96/√P` and `L·√P/2^96` back to integer reserves.
Even for a single-word degenerate range, the CL path performs
`ceil(Δ_in) + ceil(fee)` against a sqrt-price-derived Δ rather than the
V2 rational floor. So **A is a behavioral change unless a probe proves
byte-identity**, whereas **B is byte-identical by construction** because
it does not touch either simulator.

### Affected-site table (verified against the tree)

| File | Symbol / site | Current shape | Collapse impact |
|---|---|---|---|
| `src/cl/active_set.rs` | `WalkHop` (`pub(super)`) | `ConstantProduct(&IntHopState)` / `Cl { crossings, profiles }` | Center of the change. B turns it into a parametric piece descriptor; A projects the V2 arm into the CL arm. |
| `src/cl/active_set.rs` | `simulate_walk_path_inner` (forward step) | V2: `hop_state.swap(current)` one-shot floor, `landed` = 0. CL: `landed_ending_range_index` + `profile.swap` / `simulate_v3_range_swap`. | Forward dispatch site; the byte-parity crux. Under B: a per-piece forward-step capability. |
| `src/cl/active_set.rs` | `build_shifted_piece_hops` | Both arms yield `ShiftedPieceHop { hop: IntHopState, gross_input_offset, output_offset }`; CL via `crossing.ending_range.to_int_hop_state()`. | Proof the piece math/anchor is already representation-agnostic. B extends this same view to the exact step. |
| `src/cl/active_set.rs` | `walk_piece_anchor`, `walk_piece_anchor_transitional` | `#[cfg(test)]` only; enum matches for the A/B anchor baselines. | Test-only; ride the same piece view. |
| `src/cl/active_set.rs` | `single_piece_saturation_edge` | `Cl` → first range `max_gross_input_in_range()`; `ConstantProduct` → `None`. | Under B: a bounded-first-range capability query; under A the arm vanishes (always bounded). |
| `src/cl/active_set.rs` | `iteration_cap` piece-count (≈L686) | `ConstantProduct` → 1; `Cl` → `crossings.len()`. | Piece count becomes a per-piece capability. |
| `src/cl/active_set.rs` | `single_piece_path` detection (≈L712) | Same shape (`ConstantProduct` → true). | Same. |
| `src/cl/active_set.rs` | telemetry `range_counts` (≈L981) | Same shape. | Same; keep the warn payload shape. |
| `src/cl/active_set.rs` | `landed_beyond` | Calls `simulate_walk_path(...).landed`. | Indirect: depends only on `landed`, which stays per-piece. |
| `src/cl/crossings.rs` | `walk_event_first_above_predicted_inner` | Outer loop `continue`s on `ConstantProduct`; inner inversion uses `swap_exact_out` (V2) vs `cl_hop_min_input_for_output` (CL). | Second dispatch site. Under B: a per-piece exact-out-inversion capability. |
| `src/cl/crossings.rs` | `cl_walk_hop_cached`, `cl_walk_hop` | The single CL hop assembler (crossings + profiles, `Arc`-cloned). | Builder fed by the piece descriptor; unchanged otherwise. |
| `src/cl/entries.rs` | `solve_cl_piecewise` + `_inner` | Production all-CL entry: builds `WalkHop::Cl` per `ClSolveTables`, memo fingerprinted. | Stays public; constructs pieces instead of enum arms. |
| `src/cl/entries.rs` | `solve_mixed_piecewise` | Production mixed entry: builds `ConstantProduct`/`Cl` from `hop_order`. | Stays public; constructs pieces. |
| `src/cl/entries.rs` | `solve_v3_v3_piecewise`, `solve_mixed_v2_v3_piecewise` | Convenience/offline entries (tests, doc comments); build fixed 2-hop hop lists. | Not production intakes. Compile/behave only; candidates for deletion. |
| `src/cl/entries.rs` | `ClSolveTables`, memo plumbing | Prepared `Arc` tables + `walk_path_fingerprint`/`WalkMemo`. | Unaffected by the enum; the piece descriptor should carry the prepared tables. |
| `src/mixed/solve.rs` | `solve_path_inner` dispatch pyramid | `all_v2` → `exact_mobius_solve`; `all_cl` → `solve_cl_piecewise`; Solidly/weighted/stable/curve two-stage solves; else `solve_mixed_path_int`. | Only the `all_cl`/mixed walk arms move; the family two-stage branches are non-goals. |
| `src/mixed/solve.rs` | `solve_mixed_path_int` | Adapter: builds `v2_hops`, CL sequences, `hop_order`, `cl_prepared`, then calls `solve_mixed_piecewise`. | Adapter shape only. Under A the `v2_hops` projection becomes a degenerate CL sequence. |
| `src/profit_envelope.rs` | `HopMath`, `hop_lines_and_cap` | Already family-parametric (V2/Cl/SolidlyVolatile/Weighted/ReserveCap) with per-family bound lines. | Out of scope; evidence envelope-level unification already works. |
| `src/cl_cache.rs` | `ClCacheStrategy`, `PreparedHop` | Cache-lab seam: strategies refill crossing+profile tables and solve through the production entry. | Lab seam over `IntV3TickRangeSequence`, not a walk projection. Untouched. |
| `degenbot-bot/src/bot_core/resolve/mod.rs` | `all_cl_solve`, `mixed_solve` | Build `ClSolveTables` from the `ResolvedHop` `as_*` projections and call the entries. | Adapters; public entry shape unchanged. |
| `degenbot-bot/src/arb_engine/solver_capture.rs` | `shape_matches`, `serialize_hops` | Selects HeavyCl / HeavyMixed by `as_int_sequence`; serializes ranges. | Capture source for the parity corpus; reads projections only. |
| `degenbot-bot/src/arb_engine/solve_cycle.rs` | comment reference only | No `WalkHop` use. | Unaffected. |
| tests | `cl_cache_lab_goldens.rs`, `offline_heavy_replay.rs` | Drive `solve_cl_piecewise` / `derive_and_solve_cl_piecewise`. | Golden parity harness for the collapse. |
| examples | `cl_solve_replay.rs`, `mixed_solve_replay.rs`, `cl_capture_gen.rs` | Replay/generate fixtures through the entries. | Corpus generation and offline parity. |

### Discrepancies with the working survey

- Only **two** of the four solve entries are production intakes:
  `solve_cl_piecewise` and `solve_mixed_piecewise`. `solve_v3_v3_piecewise`
  and `solve_mixed_v2_v3_piecewise` appear only in tests and doc comments.
- `cl_cache.rs` is a cache-lab seam over `IntV3TickRangeSequence` /
  `PreparedHop`; it contains no `WalkHop` reference. The production CL
  projections are the `ResolvedHop::{V3,V4}` fields read through
  `as_int_sequence` / `as_crossing_table` / `as_word_profiles` and
  assembled into `ClSolveTables`.
- `walk_piece_anchor*` are `#[cfg(test)]` only, not production arms.
- `arb_engine/solve_cycle.rs` has no walk site (a comment mention of a
  cached entry only); `workload_partition.rs` reads `as_int_sequence` only.

## Options

### Option A — representation collapse

Delete `WalkHop::ConstantProduct`; project each V2 hop into a degenerate
single-range `IntV3TickRangeSequence`, so `simulate_walk_path_inner`,
`walk_event_first_above_predicted_inner`, the piece count, the saturation
edge, and the piece anchor all run the CL path unchanged. The win is one
fewer enum variant and one fewer simulator; the cost is that V2 output
rounding is now whatever the CL step math produces.

Against the code read above, this is not representation-only. The walk's
landing test (`landed_ending_range_index`) partitions `crossings` by
`crossing_gross_input`, which for the degenerate range is a single
boundary at `max_gross_input_in_range()`. A per-hop rounding shift can
move a candidate across that boundary, flip `landed`, change the visited
piece, and change `rec.profit` — an observable result change, not an
internal one. A is therefore admissible only if a byte-parity probe
shows the CL path reproduces V2 bytes exactly on the corpus.

### Option B — interface collapse

Keep both step simulators; replace the enum match sites with a parametric
per-hop piece view. The view already exists in a narrower form:
`ShiftedPieceHop { hop: IntHopState, gross_input_offset, output_offset }`
is exactly what the Möbius anchor consumes, and it is produced
identically from both enum arms. B promotes that to the whole walk by
giving each piece:

- its forward step (`V2 swap` or `CL landing + range walk`), producing the
  per-hop output and its own landed component;
- its exact-out inversion (`swap_exact_out` or
  `cl_hop_min_input_for_output`) for the event predictor;
- its piece-count / bounded-first-range capabilities used by
  `iteration_cap`, `single_piece_path`, `single_piece_saturation_edge`,
  and the telemetry payload.

The two `match` sites disappear without either simulator changing. The
"unified piecewise Möbius walk" is achieved at the seam that actually
unifies — the piece view — while the per-family EVM-exact step math stays
where it is. Per ADR-059 D6 the view must be a monomorphic
sum/trait-object-resolved-at-construction, never `dyn` dispatch inside
the per-candidate loop.

## Decision

### D1 — Provisional direction: Option B (interface collapse)

Adopt the parametric per-hop piece view and preserve the per-family step
simulators. Rationale: the rounding path difference between
`IntHopState::swap` and `compute_swap_step_v3` is structural, not an
accumulation that a probe could reasonably erase; the walk's result
depends on exact landing, so a representation swap is a behavioral change
dressed as a refactor. B delivers the stated goal (one walk, no dispatch,
unified piece view) with a byte-identical-by-construction change, which
is the standard the surrounding retrofit work already holds itself to. A
remains available and is the better end state *if* the probe clears it.

### D2 — The probe decides A; zero returned-byte divergence is the bar

A is accepted only if the byte-parity probe (below) shows the collapsed
implementation returns byte-identical `WalkOutcome.result` on 100% of the
corpus. "Byte-identical" means the returned `(optimal_input, profit,
hop_outputs)` and the derived `consumed_inputs`; internal probe values
that cannot affect the returned tuple are not compared. Under this bar,
B is the default and A must earn its way in.

### D3 — The piece view is the unification seam, not a new math layer

The view carries capability and a step function; it does not re-derive
V2 or CL math. `ShiftedPieceHop` stays the anchor-time projection; the
walk-time view may wrap it or sit beside it, but there is exactly one
place each simulator is named, and no family math is duplicated.

### D4 — Public entry surface is preserved during the change

The four `cl` entries and `ClSolveTables` keep their signatures; only
their internal hop construction changes. The engine adapters in
`bot_core/resolve` and `mixed::solve_mixed_path_int` are not part of the
change. The memo fingerprint and `WalkStats` fields keep their meaning.

## Outcome

The collapse shipped as **Option B**. `WalkHop` is gone: `PieceView` is the
sibling view of `ShiftedPieceHop` that carries each hop's forward step,
exact-out inversion, piece count, and bounded-first-range capability behind a
single family enum matched only inside the view's methods. The CL hop is
assembled in one `cl_hop_view` seam shared by both solve entries, and the
all-CL and mixed dispatch arms are consolidated into one
`solve_walkable_path_int` construction flow. Neither per-family simulator
changed: V2 still floors its rational in one DIV, and CL still runs
`compute_swap_step_v3` per target.

The byte-parity probe refuted Option A (representation collapse). Against the
zero-tolerance bar it found 18.2% of the 1,236 swap-step comparisons divergent
(10% on the real-capture subset), including real captured hops whose output
differs by hundreds of millions of wei at `x = 1` and multi-thousand-wei at
production-scale inputs. The cause is structural - the CL step floors the
post-fee net input before the invariant, and floors both the geometric-mean
liquidity and the spot price - so it is not an accumulation a probe could
erase. Evidence: `docs/architecture/v2-cl-parity-probe.md`.

The all-V2 fast arm stays on `exact_mobius_solve`. Routing all-V2 paths through
the walk's constant-product projection is neither byte-identical (83.3% of the
A/B corpus agreed overall; the 2-hop and captured subsets matched fully, while
the 3-hop grid is where it diverges) nor competitive (the walk runs ~239x
slower at the median). Evidence: `examples/all_v2_fastpath_ab.rs`.

## Non-goals

- **Solidly, Balancer weighted/stable, and Curve two-stage solves stay
  family-specific.** Their `solve_path_inner` branches and their per-hop
  `swap_fn` golden-section solves are not piecewise-Möbius over CL
  ending ranges and are untouched by this collapse.
- **The all-V2 `exact_mobius_solve` fast path is not decided here.**
  Whether it survives, or whether all-V2 paths move onto the unified
  walk, is deferred to the benchmark task's cost evidence.
- **`profit_envelope::HopMath` and the gate are untouched.** The envelope
  enum is the existing example of family-parametric unification and
  already carries V2/CL/Solidly/Weighted bounds; the walk change must not
  perturb bound lines, the clamp, or the gate.
- **No change to CL step math, crossing/word-profile builders, the memo
  fingerprint, or telemetry counter semantics.**

## Byte-parity criteria the probing task must meet

### Corpus

- **Provenance.** Production captures through `solver_capture` (HeavyCl
  and HeavyMixed) replayed by `cl_solve_replay` / `mixed_solve_replay`;
  the `offline_heavy_replay` fixtures; the `cl_capture_gen` sequences;
  and the `cl_cache_lab_goldens` golden states.
- **Coverage gate.** The corpus must span: all-CL 2-hop and ≥3-hop;
  mixed V2+CL in every interleave (V2 first, V2 middle, V2 last); CL
  word-boundary densities of 0, 1, and at least one range at
  `DENSE_OBSERVE_THRESHOLD`; single-range and multi-range CL hops;
  inputs landing exactly on a `crossing_gross_input` boundary; both
  profitable and unprofitable paths; and repeated states so the memo is
  exercised. A corpus that misses the boundary-exact cases does not
  satisfy this criterion, because that is where a rounding shift first
  changes `landed`.

### Comparisons (candidate vs status-quo baseline, per path)

1. `optimal_input` — byte equality (U256).
2. `hop_outputs[i]` for every `i` — byte equality (feeds downstream hops
   and the executor).
3. `profit` — byte equality.
4. `consumed_inputs[i]` for every `i` — byte equality (the executor-facing
   flash/swap amounts).
5. Diagnostic, recorded even on a passing run: the `landed` tuple, pieces
   visited, and sims. A `landed` difference is a fail even if the returned
   tuple happens to agree, because it means the walk's geometry moved and
   the agreement is incidental.

For fields that are not exactly equal, report the wei-divergence
distribution: count/percent of paths diverging, min, median, max, sign,
and whether the divergence is one-sided. This report is required output
of the probe regardless of pass/fail.

### Pass/fail lines (the A↔B flip)

- **Clear A** iff every path is byte-identical on all four returned
  fields, no path differs in `landed`, and the memo-on vs memo-off runs
  of the candidate agree with each other and with the baseline. A
  candidate that is byte-identical except for internal probe history that
  cannot change the returned tuple may still clear A.
- **Fail A, keep B** if on any single path: `profit`, `consumed_inputs`,
  `optimal_input`, or any `hop_outputs[i]` differs, or `landed` differs.
  The threshold is zero, not a wei tolerance: a representation collapse
  claims equivalence, so any observable divergence refutes it. A bounded
  wei divergence is not a partial pass; it is the measured cost of the
  approximation and belongs in the report, not in the accept gate.
- **Executor cross-check (both options).** For the captured paths whose
  plans are executable, re-simulate the chosen plan in the in-process
  REVM oracle and compare `consumed_inputs`/`hop_outputs` to the solver
  bytes. This catches a collapse that is self-consistent against the
  baseline but both wrong against the chain.

## Consequences

- The collapse lands as a pure interface change; the existing golden and
  offline replay suites remain the behavior guard, and no new parity
  oracle is needed to prove B safe.
- The probe's wei-divergence report becomes the evidence record for the
  A question; if it ever clears A, deleting `WalkHop::ConstantProduct` is
  a second, separately reviewed change.
- The piece view gives a single home for "what a hop can do" (forward
  step, exact-out inversion, piece count, bounded-first-range), so future
  families admitted to the walk add a capability rather than an enum arm.
- The family two-stage solves keep their own dispatch; the walk's
  monomorphism discipline (ADR-059 D6) is preserved.

## Open questions

1. Does the collapse target the all-V2 fast path as well? If all-V2 paths
   stay on `exact_mobius_solve`, A need not represent V2 at all and the
   probe's V2 corpus shrinks to mixed paths only.
   **Disposition:** No - the fast path stays. The A/B evidence shows the
   walk's all-V2 projection is divergent on the 3-hop grid and far slower, so
   `exact_mobius_solve` remains the all-V2 arm. The 3-hop divergence itself is
   tracked in the task graph, outside this ADR.
2. Corpus ownership and size: captured fixtures are state-dependent. Is
   the `cl_capture_gen` sequence set sufficient, or must the probe mine
   fresh captures (and under what chain-state conditions)?
   **Disposition:** The shipped corpus (production captures, offline replay
   fixtures, `cl_capture_gen` sequences, and golden states) was sufficient to
   settle the step-level question. Boundary-exact and multi-hop composition
   coverage stay noted as gaps for any future representation proposal.
3. Placement of the walk-time piece view: extend `ShiftedPieceHop`
   (already the anchor's view) or introduce a sibling that carries
   `landed` and the simulators? `ShiftedPieceHop` deliberately models the
   ending-range approximation only, so overloading it may blur the anchor
   versus simulation boundary.
   **Disposition:** A sibling. `PieceView` carries `landed` and the
   simulators beside `ShiftedPieceHop`, keeping the anchor's
   ending-range-approximation view separate from the simulation view.
4. Disposition of `solve_v3_v3_piecewise` and `solve_mixed_v2_v3_piecewise`:
   test-only conveniences may be deleted rather than ported.
   **Disposition:** Deleted. The two test-only entries had no production
   intake and were removed with the CL assembly unification; the two
   production entries remain.
5. If the probe clears A, is deleting `WalkHop::ConstantProduct` still a
   win once degenerate-range CL profiles are built and walked for every
   V2 hop, or does the profile build cost outweigh the variant removal?
   **Disposition:** Moot. The probe failed A on the zero-tolerance bar, so
   the constant-product arm is never deleted and no degenerate-range profile
   is built for a V2 hop.
