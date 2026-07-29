# Evaluation: combinatorial ending-range enumeration in the Möbius V3/CL solver

> Ergo task `TT4VOX`. Triggered by the V2-V3-V3 solver divergence fixture
> (`logs/fixtures/v2_v3_v3_solver_divergence_25641093.md`): a tick_spacing=1
> DAI/WETH pool whose solver-predicted WETH output over-predicted the on-chain
> (revm, byte-verified) output by ~2.7× because the `max_ranges` budget was
> consumed by `liquidity_net=0` word-boundary ticks before any initialized
> tick in the swap direction was reached.

## Verdict (TL;DR)

**Replace the combinatorial ending-range enumeration with an active-set
piecewise Möbius walk.** The recommendation is not a heuristic preference; it
follows from two facts that this document works out below:

1. The path profit function `P(x) = O(x) − x` (final output as a function of
   path input, over any mix of V2 constant-product, V3, and V4 hops) is
   **concave with a continuous first derivative**, and is **piecewise Möbius**
   (degree-(1,1) rational) in `x` — *exactly* Möbius on each ending-range
   piece, not approximately.
2. For a concave C¹ piecewise-closed-form function, the piece containing the
   argmax is found by a **monotone one-directional walk** over pieces: each
   per-piece closed-form solve tells you which side of the current piece the
   optimum lies on. No enumeration of the piece set is ever needed.

The enumeration is therefore pure waste in the common case (10³ tuples ×
(closed form + 5 simulations) for a 3-hop path, to answer a question decided
by one piece) and a silent correctness cap in the tick-sparse case
(`max_candidates = 10` truncates the piece set with no signal when the
argmax piece is outside the prefix). The `Q3YMBV` boundary-tick collapse is
endorsed as an interim patch but does not address either structural property.

Concrete follow-up tasks (files, RED tests, gates): `<REDACTED_TOKEN>`
(active-set walk), `EHSWSX` (exact affine-shifted per-piece closed form,
after `7J22EQ`), `PXSY47` (objective unification with the step-faithful
walker, after `7J22EQ`).

---

## 1. Where the design lives today

Hot path: `arb_engine/solver_dispatch.rs` resolves each hop; V3/V4 hops call
`V3PoolState::build_int_v3_sequence(tick_spacing, fee, zfo, 10)`
(`rust/crates/degenbot-pools/src/v3_state.rs`), which slices
`get_cached_tick_ranges(..., max_ranges=15)` (`compute_tick_ranges` in
`tick_bitmap.rs`) down to an `IntV3TickRangeSequence`
(`rust/crates/degenbot-pools/src/int_v3_hop.rs`). The all-CL dispatcher calls
`int_solve_cl_path` (`rust/crates/degenbot-solvers/src/mobius_v3_int.rs`);
mixed V2+CL paths go through `exact_solve_mixed_path_n` /
`exact_solve_mixed_v2_v3_sequence`. All of these enumerate ending-range
tuples: `max_candidates = 10` per CL hop, mixed-radix counter, per-tuple
`exact_mobius_solve` on the *unshifted* ending-range `IntHopState`s, then
`total_optimal_input = piecewise_optimal + Σ crossing_gross_input`, then a
±2 neighbor sweep validated by the piecewise simulator
(`int_simulate_cl_path_n` / `int_simulate_mixed_path_n`).

Three separate caps sit on the same data flow:

| Cap | Site | What it truncates | Failure mode when hit |
|---|---|---|---|
| `gen_ticks` budget (`max_ranges*4+16`) | `compute_tick_ranges` | raw tick walk | silent short walk |
| `max_ranges = 15` | `get_cached_tick_ranges` | cached boundary list | starvation by zero-net boundary ticks (the fixture) |
| `max_candidates = 10` | `int_solve_cl_path` + mixed solvers | enumerated piece prefix | argmax piece silently not enumerated |

All three degrade silently. None reports "model truncated"; the solver
returns a confidently wrong answer (3032343697 WETH vs 1109518347 actual).

## 2. The mathematical structure the solver actually optimizes

### 2.1 One V3 range is exactly constant-product in virtual reserves

Within a tick range with liquidity `L` and current `sqrtPriceX96`, the
gross-input ↔ output relation is exactly the V2 form with virtual reserves
`R₀ = L·2⁹⁶/√P`, `R₁ = L·√P/2⁹⁶` (this is what
`IntV3TickRangeHop::compute_virtual_reserves` computes). So a single range's
output map is the Möbius map `y = γ·s·x / (D·r + γ·x)`.

### 2.2 Crossing a tick: piecewise Möbius, C¹, concave

For a V3 pool, output-vs-input `o(x)` is piecewise Möbius with breakpoints at
the inputs that reach range boundaries. At any breakpoint the **spot price is
continuous** (a tick crossing changes `liquidity`, not the price). The
marginal exchange rate `do/dx` is `γ ×` (a monotone function of the current
spot price within each range), so:

- `do/dx` is **continuous across the breakpoint** — the liquidity change
  alters only how *fast* the rate falls off (second derivative jumps, stays
  negative on both sides);
- `do/dx` is non-increasing everywhere within each range (price only moves
  adversely in the swap direction).

Hence `o(x)` is **concave and C¹** on its whole domain, whether the next
range is deeper or shallower. (This is the smooth-model statement; EVM floor
rounding perturbs it at wei scale — the same staircase the existing ±2 sweep
already patches, see §6.)

### 2.3 Chaining hops preserves the structure

Composition of increasing concave functions is concave; V2 hops are smooth
concave Möbius. Derivative continuity composes by the chain rule. Profit
`P(x) = O(x) − x` is therefore a **concave, C¹, piecewise Möbius** function
of the path input, with breakpoints wherever *any* hop crosses a range
boundary. Each maximal interval between breakpoints corresponds to exactly
one ending-range tuple `k = (k₁, …, kₙ)`.

### 2.4 Each piece is *exactly* Möbius — the enumeration was approximating an exact structure

The crossing offsets are input-independent constants (this is already
documented on `IntTickRangeCrossing`). Given a tuple `k`, with per-hop
gross-input offsets `gᵢ` and output offsets `oᵢ`:

```
u₁ = x − g₁            y₁ = m₁(u₁) + o₁
u₂ = y₁ − g₂           y₂ = m₂(u₂) + o₂
...
O(x) = mₙ(uₙ) + oₙ
```

Möbius maps are closed under pre-translation (`u = x − g`), post-translation
(`+ o`), and composition (SL(2,ℝ) closure; concretely, 2×2 integer matrix
multiplication). So on each piece, `O(x) = (A·x + B) / (C·x + D)` with
integer `(A, B, C, D)` computable directly, and the piece argmax is closed
form:

```
P'(x) = (A·D − B·C) / (C·x + D)² − 1 = 0   ⟹   x* = (√(A·D − B·C) − D) / C
```

(one `isqrt_u512`, same discipline as `compute_mobius_model_optimal_input`;
with `B = 0` this reduces to the existing `x* = (√(K·M) − M) / N`). The
existing `compute_int_mobius_coefficients` is already this 2×2 matrix
recurrence minus the translation entries, in the same `U512` width.

**Consequence:** the piecewise extension does *not* stray from the Möbius
closed form's exactness. If anything, the *current implementation* is the
approximation: it composes coefficients from the unshifted ending ranges and
then adds `Σ crossing_gross_input` to the result. For hops ≥ 2 the crossing
is spent from an upstream hop's *output*, not the path input, so the true
input shift involves thresholding through the preceding hop's map — the
additive shift misanchors the per-tuple candidate by `O(crossing × price
impact)`, far beyond the ±2 sweep's reach. Only the validation sims keep the
enumerated candidates honest; the "closed form per tuple" is today a
heuristic anchor.

## 3. Why the enumeration design fails — the structural reading

With §2 in hand the three failure modes of the enumeration collapse into
one: **it uses brute-force subset enumeration to locate the argmax piece of
a function whose piece structure is monotone.**

1. **Wasted work, exponentially in hop count.** For a well-behaved pool the
   argmax piece is almost always `(0..2, 0..2, …)` — arbs are small relative
   to pool depth. The enumeration still runs up to `10ʰᵒᵖˢ` closed forms +
   5× that many piecewise simulations. The active-set walk visits exactly the
   pieces between input 0 and the argmax, typically 1–3 iterations.
2. **Silent correctness cap.** `max_candidates` truncates the piece *prefix*.
   If the argmax piece lies beyond the prefix, the solver does not know. Worse,
   it composes with the `max_ranges` starvation: when the prefix consists of
   zero-`liquidity_net` boundary ranges (the fixture), every enumerated piece
   models constant liquidity, so the solver predicts the no-slippage output.
   Concavity does not save you from enumerating over the wrong model.
3. **The budget is not load-bearing for the math.** Nothing in the closed
   form or the piece geometry needs the enumeration; the cap exists only
   because the walk that would make it unnecessary was never built.

## 4. Recommended design: `solve_cl_path_active_set`

Replace the mixed-radix loop in `int_solve_cl_path`, `int_solve_v3_v3`, and
`exact_solve_mixed_path_n` / `exact_solve_mixed_v2_v3_sequence` with one
active-set routine (V2 hops simply have piece-count 1 and no crossing data):

```
hypothesis k = (0, …, 0)                      # all hops in base range
best = None
loop at most Σᵢ rangesᵢ iterations:           # hard cap; see §5
    anchor x̂ = per-piece closed form on tuple k   (§2.4, or transitional
                                                   approximate anchor §4.1)
    simulate path O(x̂) with the validation simulator
    record (x̂, O(x̂) − x̂) into best if better
    k' = ending tuple that the walk actually landed in
    if k' == k and x̂ is strictly interior to piece k's input extent:
        break                                  # C¹ concave ⇒ global argmax
    k = k'
±2-exact sweep around best.x with real simulation; return best
```

### 4.1 Transitional anchor

`7J22EQ` may ship with the *existing* per-tuple anchor (unshifted
coefficients + additive crossing cost) inside this loop. The loop's
correctness does not depend on anchor precision — the simulation tells the
truth about which piece `x̂` landed in, and the monotone walk converges
regardless; a worse anchor only costs extra iterations. `EHSWSX` then swaps
in the exact affine-shifted coefficients of §2.4 (`U512` matrix composition
with translation entries, argmax `(√(A·D−B·C) − D)/C`), making each piece
step exact and typically collapsing the walk to 1–2 iterations.

### 4.2 Convergence argument

The breakpoints partition `[0, ∞)` into intervals; on interval `I_k` the true
`P` coincides with piece `k`'s closed-form extension, whose derivative is
decreasing (concavity). If piece `k`'s argmax lies at or beyond the right
edge of `I_k`, then `P' ≥ 0` throughout `I_k`; by C¹ the adjacent piece's
extension also starts with `P' ≥ 0`, so the optimum lies strictly to the
right — the walk never needs to return left. Symmetrically for the left
edge. Therefore the walk visits a strictly monotone sequence of pieces and
terminates in the argmax piece after crossing at most as many breakpoints as
lie between 0 and the argmax — bounded above by `Σᵢ rangesᵢ`, which is the
loop cap. Pool exhaustion ("argmax beyond modeled liquidity") is the
walker hitting its last modeled piece; it degrades to "largest validated
candidate", exactly today's semantics.

### 4.3 What happens to the three caps

- `max_candidates` **dies with the enumeration**. There is no tuple budget.
- `max_ranges = 15` becomes a **cache/memory bound only**, not a correctness
  cap. The active-set solver consumes ranges on demand in walk order (the
  same `gen_ticks` discipline as `v3_simulate_swap`, with sparse-mode
  `MissingTickWord` propagation); a tick-sparse pool with 40 boundary words
  and an initialized tick 10,354 ticks away is fully reachable — the fixture
  failure mode disappears structurally, not by re-shaping the filter.
- `gen_ticks` budget remains as a work ceiling, now hit only by genuinely
  deep walks, not by boundary-tick interleaving arithmetic
  (`max_ranges*4+16`).

`PXSY47` then unifies the validation objective with the step-faithful
sequential walker (`v3_simulate_swap`'s `compute_swap_step_v3` loop
discipline, generalized to an N-hop path), replacing the bespoke piecewise
objectives (`int_simulate_v3_swap` / `int_simulate_cl_path_n` /
`int_simulate_mixed_path_n`). This retires a whole dual-maintenance seam:
today both the piecewise simulator and `compute_crossing` re-derive the
per-boundary rounding of `computeSwapStep` (the `ON5QMD` parity), in a second
codebase, with their own parity tests. After unification the word-boundary
`computeSwapStep` flooring exists in exactly one place — the walker — which
is also the Tier-3 byte-exact oracle path. The `Q3YMBV` collapse filter
(boundary ticks flanking initialized ticks) keeps its value for the cached
range list used by non-solver consumers, but the solver no longer depends on
boundary-tick presence for rounding parity.

### 4.4 Performance profile

Per candidate path: today `O(∏ min(10, rangesᵢ))` closed forms + `5×` that
many simulations; after: `O(walk depth)` iterations × (1 closed form + 1
walk) + a constant final sweep. Walk depth for real arbs is 0–3 ranges per
hop. This is roughly two orders of magnitude cheaper on 3-hop paths and
scales linearly rather than exponentially with hop count — safe headroom to
widen path-finding to longer paths. Building sequences also gets cheaper:
`compute_crossing` is only needed for pieces actually visited (and can be
incremental in `k`).

## 5. Alternatives considered

- **Status quo + `Q3YMBV` collapse.** Patches the mid-tier cap (boundary
  starvation) but leaves the enumeration's cost blowup and the silent
  `max_candidates` truncation intact, and keeps the approximate per-tuple
  anchor. Rejected as the end state; endorsed as the interim fix (it is
  independently correct and nearly complete in the working tree).
- **Hybrid: enumerate when cheap, walk when sparse.** Two solvers to keep in
  parity; the walk dominates the enumeration in the cheap regime too (the
  single-range fast path is just the first iteration of the walk). Rejected —
  one mechanism, no regime switch.
- **Bisection on the marginal-profit sign.** Simulate at `x` and `x+h`, sign
  of `(O(x+h) − O(x))/h − 1` brackets the argmax of a concave staircase;
  `O(log U256)` iterations is too many simulations, and the wei-scale
  staircase perturbs the finite-difference sign near the optimum. Kept as a
  documented fallback safety net inside the iteration cap of
  `solve_cl_path_active_set`, not as the primary mechanism.
- **"Lagrangian/KKT-style" solver.** This is what the active-set walk *is*:
  the piece identity is the active constraint set; complementary slackness is
  exactly the rule "the argmax piece is the one whose unconstrained closed-form
  argmax lies strictly inside it"; the walk's direction test is the KKT
  sign check. The recommendation formalizes the task's KKT alternative as
  the sequential-walk alternative.

## 6. Risks and edge cases

- **Wei-scale staircase.** EVM floor rounding makes discrete `P` not exactly
  concave; the discrete argmax can jog by a couple of wei, and a true optimum
  within rounding noise of a piece boundary sits in "both pieces". The final
  ±2 sweep (same discipline as `exact_mobius_solve`) plus "best validated
  candidate" tracking covers this; the walk's termination check must use the
  *interior* test, not exact-argmax equality.
- **Anchor overshoot with the transitional anchor (`7J22EQ`).** The
  approximate anchor can land several pieces off when crossing costs are
  large (downstream shifts are mispriced). Handled by the loop itself —
  convergence is driven by simulation feedback, and the iteration cap +
  best-so-far tracking bounds pathological anchors. `EHSWSX` removes the
  misanchoring at the root.
- **`U512` width on deep paths.** The §2.4 determinant composition inherits
  the same width discipline as `compute_int_mobius_coefficients`; at ≥5 hops
  the coefficient products strain `U512`. Out of scope for `EHSWSX` — the
  current solver has the identical exposure — but `EHSWSX` must carry a
  width comment and a debug-assert mirroring the existing `U512 → U256`
  narrowing asserts.
- **Partial-fill / exhaustion semantics.** `int_simulate_cl_path_n` has an
  "input below crossing_gross_input → path exhausted" branch that returns
  zeros mid-path; the walker handles exhaustion by mirroring the contract's
  price-limit stop. `PXSY47` must preserve the solver-visible distinction
  between "no profitable input" and "pool exhausted".
- **`ON5QMD` regression risk during transition.** Until `PXSY47` lands, the
  objective remains the piecewise simulator whose rounding parity is covered
  by `int_v3_hop.rs`'s `compute_crossing_matches_onchain_step_walk_for_
  multi_range` and by `v4_word_boundary_solver_divergence.rs`. These stay
  green at every step; `7J22EQ` changes *which candidates are proposed*, not
  *how candidates are validated*.

## 7. Validation / RED-test plan for the follow-up implementation

1. **Fixture RED (`7J22EQ`):** reconstruct the block-25641093 V3 DAI/WETH
   pool state from `logs/fixtures/v2_v3_v3_solver_divergence_25641093.md`
   (`sqrt_price_x96=1956421190421993762013571523`, `tick=-74028`,
   `liquidity=5407362545736161987`, 12 initialized ticks, nearest lower
   initialized tick −84382) and assert the solver's predicted hop-2 output
   for input `4973433520019` DAI zfo is within wei tolerance of
   `1109518347` — not `3032343697`. RED on the enumeration (starved
   sequence), GREEN with the active-set walk consuming the full tick data.
2. **Property test:** for randomized pool states (including tick_spacing=1
   with initialized ticks ≥ 20 word boundaries apart — sparse topologies
   crossing uninitialized word boundaries) and randomized directions, the
   active-set solver's returned profit must ≥ the profit of *any* tuple the
   old enumeration would have validated within its `max_candidates` prefix —
   i.e. never worse than the old design — and must equal
   `max over a fine sample grid` within the staircase tolerance.
3. **Iteration-count guard:** solver performs ≤ a small multiple of
   `Σ ranges` closed forms (assert via a counter in test builds) — this is
   the regression net against accidentally re-introducing combinatorial
   behavior.
4. **Keep green:** `mobius_v3_int.rs` unit tests, `int_v3_hop.rs` crossing
   parity tests, `v4_word_boundary_solver_divergence.rs`, the
   `tests/standalone_parity` V3/V4 dual-driver pairs, and the Tier-3
   `just test-tier3-*` nets.
5. **`PXSY47` specific:** byte-exact agreement of the new objective with
   `v3_simulate_swap` on the fixture state, and with the revm canonical-bytecode
   V3 `Pool.swap` oracle (Tier-3) on pinned mainnet slots.

## 8. As-built outcome (ergo 7J22EQ, landed)

The combinatorial enumeration was removed from `int_solve_cl_path`,
`int_solve_v3_v3`, `exact_solve_mixed_v2_v3_sequence`, and
`exact_solve_mixed_path_n`; all four now delegate to
`solve_active_set_path` in `mobius_v3_int.rs`. `max_candidates` is gone.
The as-built walk differs from the §4 sketch in ways the implementation
surfaced:

1. **The enumeration was also corner-blind** (a second failure mode found
   during RED-fixture construction). The per-piece anchor models the ending
   range as an UNBOUNDED constant-product pool; when the true optimum is
   "fill the piece to its saturation boundary" the anchor overshoots, the
   validation sim reports negative profit, and even an UNcapped enumeration
   finds nothing on geometries with real profit (e.g. 2.5e7 wei on the
   mixed test fixture). Fix: the walk computes each visited piece's input
   window with two monotone bisections over the landed-tuple map
   (componentwise non-decreasing in x) and refines the stop-region pieces
   with a windowed ternary search + dense sweep (`walk_refine_window`) —
   the two-layer discipline of `exact_mobius_solve` generalized to windows.
2. **The EVM floor staircase rules out pointwise direction tests.** The
   discrete profit is flat to ±1 wei over tens of wei around the top, so
   neither "argmax hugs the edge" nor pointwise edge-score comparisons are
   reliable. The as-built direction test uses straddle probes at ±64 around
   the right edge with +1-wei tolerance; on stop it refines the current
   piece AND its forward neighbor (a peak straddling the edge is never
   mis-attributed).
3. **One-piece steps, no hopscotch** in the transitional version: the walk
   visits consecutive pieces only, which by concavity cannot vault the
   peak. Exact anchors (`EHSWSX`) re-enable hopping past clearly-climbing
   pieces.
4. **Perf (debug build, per solve):** single-range 2-hop ≈ 11 µs (was the
   old fast path's ~handful of sims); 8-range 2-hop ≈ 5.1 ms vs ≈ 2.7 ms
   for the legacy enumeration on the same shape. Slower in debug for
   multi-range, structurally cheaper for tick-sparse pools (no tuple
   budget); the release profile collapses the gap by an order of magnitude.
   `EHSWSX` owns the remaining gap (exact anchors ⇒ fewer probes).
5. `int_simulate_v3_swap` now saturates at the range boundary on absurd
   inputs instead of panicking on the U512→U256 narrowing (window-edge
   probes can propose domain-scale inputs the enumeration never tried).
6. Guard instrumentation: thread-local `WALK_PIECES_VISITED` /
   `WALK_PATH_SIMULATIONS` counters with a test asserting
   `pieces ≤ Σ ranges + 2` and sims bounded per range.

Validation on landing: 72 lib tests + the V4 parity nets
(`v4_word_boundary_solver_divergence`, `v4_crossing_solver_vs_sim_parity`)
+ `degenbot-bot` lib (277) + umbrella `parity_v3_swap`/`parity_v4_swap`
green; clippy `-D warnings` clean.

## 9. Cross-references

- Ergo: `TT4VOX` (this evaluation), `Q3YMBV` (interim boundary collapse),
  `7J22EQ` -> `EHSWSX`, `7J22EQ` -> `PXSY47` (sequenced follow-ups),
  `ON5QMD` (rounding parity),
  `BQ43DK` (Tier-3 enforcement).
- Fixture: `logs/fixtures/v2_v3_v3_solver_divergence_25641093.md`.
- Code: `rust/crates/degenbot-solvers/src/mobius_v3_int.rs`,
  `rust/crates/degenbot-solvers/src/mobius_int_exact.rs`,
  `rust/crates/degenbot-solvers/src/mobius_int.rs`,
  `rust/crates/degenbot-pools/src/int_v3_hop.rs`,
  `rust/crates/degenbot-pools/src/tick_bitmap.rs`,
  `rust/crates/degenbot-pools/src/v3_state.rs`
  (`build_int_v3_sequence`, `v3_simulate_swap`),
  `rust/crates/degenbot-cl-math/src/cl_lib/swap_math.rs`.
- Prior diagnosis doc: `docs/architecture/sim_v4_swap_step_rounding.md`
  (the `int_simulate_v3_swap` 2-range approximation finding — `PXSY47`
  retires exactly that seam).
