# ADR-108: Bound-certified search is scoped to piecewise-Möbius classes

**Status: proposed (research complete; no production cutover).** (2026-11-26;
ergo task `J24WJQ`.) Extends the optimality-contract clarification in
`docs/architecture/mobius_v3_ending_range_enumeration_evaluation.md` and the
black-box hook boundary in [ADR-090](ADR-090-v4-hook-black-box-finalist-requote.md).
This ADR records a class boundary and measured prototype results; it does not
change a solver, add a runtime stance, or introduce a compatibility layer.

## Context

The concentrated-liquidity active-set walk obtains directional evidence by
comparing observed scores. That rule is intentionally free of a fixed wei
tolerance, but global optimality still depends on the smooth concave,
unimodal model used by refinement and climbed-piece skipping
(`solve/concentrated_liquidity/active_set.rs`). The direction probes do not
bound an unvisited region, so they cannot certify an arbitrary non-unimodal
curve.

This is not the only unqualified search contract. Solidly, Balancer weighted,
Balancer stable, and Curve stableswap use `solve_golden_section_path_int`,
documented as a unimodal search on the real leaf
(`solve/mixed/solve.rs`). Curve stableswap has measured non-concave
counterexamples (`docs/architecture/stableswap-gate-bounds-and-solve-method-evaluation.md`).
M8 does not silently change any of those arms.

The impossibility is informational, not a tuning problem. Given only a finite
set of score observations, another admissible curve can agree at every probe
and contain a higher point elsewhere. A finite `U256` domain is enumerable in
principle but not tractably exhaustible (~1.16e77 points). Shubert's and
Piyavskii's sequential methods therefore assume a rate-of-change bound;
interval branch-and-bound likewise requires an interval extension or another
admissible regional bound. Primary references and protocol sources are
indexed in `.scratch/mobius-nonunimodal-research.md`.

The tree already has three relevant mechanisms:

- `RefineBound` and `clamp_window_to_bound` express and solve the exact
  integer predicate `(a + b·x)/c ≥ best` for a region that can still beat the
  incumbent (`solve/concentrated_liquidity/window.rs`).
- The shifted-piece composer gives an exact smooth Möbius supremum within one
  ending-range piece (`solve/mobius_shifted_piece.rs`). Today that value is a
  heuristic anchor, not the authority that terminates the search.
- `path_profit_bound` already computes a rigorous upper bound on
  `max_x [path_output(x) - x]`: `finalize_bound` takes the discrete concave
  maximum of the composed lower-envelope lines over `PathBound.xmax`
  (`solve/profit_envelope/gate.rs`, `solve/profit_envelope/kernel.rs`). The
  walk supplies a realized lower bound, so the cheapest next measurement is
  their existing gap distribution, not another solver.

The question is whether a tighter interval mechanism can close that gap for
non-unimodal curves without making production search uneconomic.

## Decision

### D1 — Name the domain and separate evidence, bounds, and certificates

Every verdict names its input domain: `[lo, hi]` plus evidence that the tail
above `hi` cannot improve the incumbent, or an explicit uncovered-tail
refusal. With complete tick data in the swap direction, one hop's
`bounded_tail` saturation is enough to bound a composed path; the walk's
all-hops `model_domain_exhausted` conjunction is stricter than a certificate
requires. Sparse data does not supply this evidence. A domain end at
`U256::MAX` is also explicit; an arbitrary two-times-optimum cutoff is not a
global certificate.

These are three distinct outcomes and must not be conflated:

- **Directional evidence** ranks observed regions. It guides the walk but says
  nothing about an unprobed spike.
- An **admissible interval upper bound** dominates every realized score in a
  stated interval. It may be loose; looseness affects cost and gap closure,
  never soundness.
- A **certificate** pairs a realized lower bound with an admissible upper
  bound and proves `upper ≤ lower` (or the agreed integer gap) over the whole
  named domain, including the tail. Cap exhaustion, unavailable bounds, and
  model gaps are uncertified outcomes, never successful optima.

A certified search therefore returns at least
`[domain, best_x, realized_profit, certified_upper, gap, verdict]`, where
`verdict` distinguishes `Certified`, `BudgetExhausted`, and
`Refused(model_gap)`.

### D2 — Certification is limited to families with a valid partition bound

The sufficient structure is an enumerable partition plus a closed-form or
interval upper bound that pointwise dominates the realized integer chain on
every node. A point test at the entry is not enough for an interval claim.

| Family | Bound position | Certification position |
| --- | --- | --- |
| V2 constant product | One exact concave Möbius piece | Exact model optimum, subject to the realized floor-jog contract |
| White-box V3/V4 concentrated liquidity | Exact Möbius supremum per ending-range piece | Certifiable with complete partition coverage and tail evidence; incomplete tick data, cap exhaustion, overflow, or amount-modifying hooks invalidate the claim |
| Balancer V2 weighted | Concave entry tangent and asymptotic cap from the official weighted invariant | Candidate bound; extend the existing small-scale dominance proptest into the unchecked wrap regime and measure interval tightness |
| Solidly stable | Tangent when its faithful-arithmetic guard passes; cap-only otherwise | Admissible only under the same guard; ≈89% of randomized dominance-proptest shapes are cap-only, so economic closure is unlikely |
| Balancer V2 stable | Curvature is established at the entry today | Conditional: require curvature over the whole interval or fall back to cap-only; the live global tangent also needs this audit |
| Curve stableswap | Measured non-concave counterexamples; reserve cap remains sound | Cap-only and generally uneconomic; no unqualified global certificate |
| Amount-modifying V4 hooks, Balancer V3 custom pools/hooks | Arbitrary state or invariant behavior | No white-box certificate; retain black-box exact re-quote/refusal |
| Curve cryptoswap | Unmodelled in this tree | No bound today; do not infer impossibility for all future models |

This boundary applies to the realized integer chain, not only a real-valued
smooth model. Incomplete partition coverage, sparse tick data, arithmetic
undercut, hook side channels, and unsupported leaves are model gaps and poison
the certificate. The certified-convex lab's historical 16-range window gap is
not a claim that today's production walk still has a `max_ranges` cap.

### D3 — Do not migrate to the measured naive interval scheme

The scratch prototype in `.scratch/nonunimodal-bb/` implements best-first
interval search. Its synthetic oracle has a local maximum of 128 at input 64
and a global maximum of 192 at input 192, with maximum unit-step magnitude 3.
A scalar direction-only analogue stops at 128 after five probes; best-first
certifies 192 in 19 evaluations on `[0,255]`.

For the two golden-bearing paths in
`concentrated_liquidity_capture_offline.jsonl`, the experimental domain is
`[0, max(2 * golden_input, 1_000_000)]`. This is a cost experiment, not a
full-domain certificate. The prototype seeds with the **recorded golden
input**; on both rows its profit equals the walk's profit, so the incumbent is
the walk incumbent. A stronger golden incumbent would only prune more and
make this no-go more conservative. Exact scoring uses
`ConcentratedLiquidityPathSim`.

For `[lo, hi]`, let `b = path_output_bound_at(hi)` and apply the gate's
mandatory soundness allowance `b + floor(b/2048)` before subtracting `lo`.
The envelope dominates output pointwise up to the reduction deficit covered
by that allowance, its `Line` slopes satisfy `B ≥ 0`, and a pointwise minimum
of nondecreasing lines is nondecreasing, so the interval's largest output
bound is at `hi`; the `-x ≤ -lo` term completes the argument. This bound is
sound within the gate's slack contract and the prototype's stated
`u128`-saturating limitations, but weaker than the existing whole-domain
concave maximizer and not interval-tight. Bound derivation is recomputed per
interval, and each run is capped at 2,000 iterations.

Debug-profile measurements:

| Case | Walk result and cost | Best-first outcome |
| --- | --- | --- |
| Synthetic | 128 at 64, 5 probes | **Certified 192 at 192 on `[0,255]`**, 19 evals, 35 bound calls, 46 us |
| Offline path 4 | profit 293672959484049194114282; 1,188 sims; 36,838 us | Same profit; **uncertified** after 2,000 iterations, 1,588 evals, 3,173 bound calls, 2,151,934 us |
| Offline path 105 | profit 3889035615208277603; 3,430 sims; 53,984 us | Same profit; **uncertified** after 2,000 iterations, 1,019 evals, 2,035 bound calls, 10,953,779 us |

The captured scheme preserves the golden/walk answer but closes neither
certificate at a practical budget. Repeated bound derivation is the inferred
dominant cost from the counters and total wall time, not a separately measured
profile. There is therefore **no production cutover to this weaker-than-gate
endpoint scheme**. This is a measured rejection of that scheme, not a claim
that every piecewise-Möbius certificate is impossible. The full method and
commands are in `.scratch/nonunimodal-bb/REPORT.md`.

The existing gate already provides a sharper first measurement. On these two
rows its whole-domain upper bounds and gaps are:

| Path | `path_profit_bound` | Walk profit | Upper-minus-lower gap | Added slack | Slack-adjusted residual |
| --- | ---: | ---: | ---: | ---: | ---: |
| 4 | 293816913034494672523085 | 293672959484049194114282 | 143953550445478408803 | 143395272344799742568 | 558278100678666235 |
| 105 | 1717762310270242152902 | 3889035615208277603 | 1713873274655033875299 | 838341781488649171 | 1713034932873545226128 |

The gate's domain is `[0, xmax]`, where `xmax` is the sum of per-hop output
caps. Above `xmax`, final output is at most the last hop's cap and therefore
no greater than input, so profit is non-positive and below the gate's
non-negative skip floor. The tail is covered by that argument, not by an
unbounded search.

Those bounds are rigorous but include existing reduction slack and are not
1-wei certificates. Path 4's slack is 99.61% of its reported gap, so materiality
must be judged from the slack-adjusted residual; path 105's residual remains
unusable. These two offline rows are disjoint from the 393-row corpus used for
the migration gates below.

### D4 — Measure the existing gap first, then prove exact-per-piece leapfrogging

A later epic starts with **G0**, a no-new-solver measurement:

- Compute floor-free `path_profit_bound - walk_profit` over 369
  `heavy_mixed_solve_captures` occurrences (27 unique paths) plus 24
  `live_gatebursts_mixed` occurrences (393 total), reporting raw and
  slack-adjusted gap distributions and naming each unsupported exit. Report
  occurrence-weighted and unique-path coverage separately. Do not use
  `path_profit_bound_with_floor`: its early exit can return below the true
  bound and would make the gaps incomparable.
- Measure the same quantity on separately named Balancer, Solidly, and Curve
  corpora before making any family-specific migration claim; the 393
  occurrences are V2/concentrated-liquidity only and cannot exercise those
  family rows.
- If the existing envelope already covers every declared synthetic second
  hump, record that falsifier and keep M8 verification-only.

Only if at least one captured path has a **slack-adjusted residual greater
than `PROFIT_EPS` (100,000 wei)** should the next prototype combine. That
threshold measures bound slack; a solver win still requires the G2
over-shoot criterion below.

1. **Leapfrog pruning.** `clamp_window_to_bound` already implements both
   positive- and negative-slope cuts. The new work is exposing its window/line
   data to a lab-side **first-crossing search** for the first later input whose
   affine bound can still beat the incumbent. Piece ordering alone cannot
   help: the piece lattice is totally ordered by input.
2. **Exact per-piece node bounds.** The tree exposes the shifted-piece
   argmax input and slope predicates, but no value/supremum evaluation API. A
   node key needs that API to clamp the argmax input to the node interval and
   evaluate the supremum with integer-ceiling semantics. This bound, not the
   composed-lines `best/2048` reduction slack, is the candidate for a 1-wei
   closure target. Any existing tangent or
   per-piece undercut of the realized chain is a P0 soundness finding.
3. **Certified termination and tail accounting.** Stop only when every open
   interval **and the remaining tail** has upper bound at most the realized
   incumbent. Name `IncompletePartition`, `MissingTickWord`, `DomainOverflow`,
   `UnsupportedLeaf`, and `BoundInadmissible` as model gaps; report the
   remaining gap on every uncertified exit.

Production entry then requires all of:

- **G1:** closed certificates (gap ≤ 1 wei) on at least 95% of the 393
  V2/concentrated-liquidity occurrences, with no under-shoot beyond
  `PROFIT_EPS`;
  every over-shoot beyond `OVER_SHOOT_TOLERANCE_WEI` is either a named G2
  improvement or a P0 soundness finding;
- **G2:** oracle global maxima on 100% of the synthetic non-unimodal corpus
  against the explicitly labelled scalar direction analogue, and at least one
  captured production-path improvement greater than
  `OVER_SHOOT_TOLERANCE_WEI` (8 wei); synthetic-only wins do not justify
  production migration;
- **G3:** at most 1.5× the walk's node/piece visits and no more simulations
  on the same 393 occurrences, reported with unique-path coverage;
- **G4:** median compose-plus-isqrt cost at most 0.1× one path simulation;
- **G5:** median and p99 latency within +10% of the walk on the same corpora.

The same closure and cost criteria must pass on separately named family
corpora before a non-V2/CL family migrates. An 80–95% closure result remains
prototype-only; below 80% with no tighter admissible provider is a structural
no-go. Other kill conditions are more than 4× node visits, `BigInt` fallback
`BigInt` fallback on more than 1% of nodes on the ≤4-hop subset of the
393 occurrences (also reported per unique path), every production
improvement bounded by `OVER_SHOOT_TOLERANCE_WEI`, or required participation
by a no-bound family.

If all applicable gates pass, migration is verifier-first: solver-lab
certification, then an independently reviewed runtime stance, then hard
cutover and stance removal. No production implementation is authorized by
this ADR alone.

## Consequences

- The active-set walk remains the production concentrated-liquidity search.
  Its documentation continues to distinguish useful direction evidence from
  the smooth-model optimality contract.
- The golden-section family arms are also unchanged. In particular, M8 does
  not claim to fix Curve's measured non-concavity or any other non-CL leaf.
- Arbitrary curves are not assigned a global-optimality guarantee. Where no
  admissible bound exists, approximate search plus exact black-box re-quote
  remains the honest answer; ADR-090's hook policy is unchanged.
- The [certified-convex scratch effort](../../.scratch/certified-convex/PROMOTION.md)
  and this decision are complementary: the former certifies within a convex
  model/window, while D4 concerns the optimum across the ending-range
  partition. The scratch effort is evidence, not a ratified production ADR.
- M8 delivers research, a reproducible counterexample, and a measured no-go
  for the naive bound. It delivers no production solver and no flag.

## Evidence

- `.scratch/mobius-nonunimodal-research.md` — primary literature, official
  protocol sources, family-by-family bound analysis, and source index.
- `.scratch/nonunimodal-bb/REPORT.md` — method, exact measurements,
  limitations, and migration criteria.
- Reproduce the five oracle/counterexample tests and captured comparison:

  ```sh
  just --justfile .scratch/nonunimodal-bb/justfile test
  just --justfile .scratch/nonunimodal-bb/justfile run
  ```
