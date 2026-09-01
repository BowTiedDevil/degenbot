## Fork: the right-edge bisection budget cannot be reduced in x-space

Context: user asked to improve the climb algorithm using captured goldens +
rigor. This is the measured conclusion of the attempt (loop-14).

### Attempt 1 — closed-form prefix inversion (measured, rejected)
Compute the piece crossing as min_i of the Möbius prefix-chain inverse at each
hop's next boundary gross input T_i. Result (replay of 234-piece family 3671
[318,88,291]): 1/233 pieces localize — at the predicted x the realized chain
sits 1–2 wei BELOW target and the landing index does not roll over until the
NEXT lattice preimage, ~a piece width away (1e16–1e19 wei). Quantization is the
exact +2 wei deficit that hides the first event; any seed grows through the
whole span anyway. Right-edge sims/piece: 69 event-driven vs 68 baseline.

### Attempt 2 — recursive per-stage inversion analysis (the floor-cancel lemma)
For integer T, floor(f(x)) ≥ T ⟺ f(x) ≥ T — so per-hop floors cancel under
threshold queries, and a one-level per-hop demand inverter (profile prefix
binary search + full-math ceiling inverses: price→delta→input→fee) would give
EXACT firing times. This appears to kill the grow+bisect loop.

Why it does not: the measured hop-output deficit is not per-stage: it
ACCUMULATES over x (the V3 exact-in muldiv floor and word-boundary slab
rounding drop output versus the real-domain Möbius model by amounts growing
with the input magnitude). At 4.3e17 wei, the deficit is 2 units with the
real crossing ~1e9 wei beyond the model closure point — non-local drift, for
any per-event algebra. Building a correct event-queue oracle requires a real
integer-V3 chain composition (a discrete-integration layer over
compute_swap_step: nested ceilings of getNextSqrtPrice/amount delta
inverses across slab graphs), which replaces `simulate_walk_path` itself with
an equally expensive exact arithmetic engine: same log-span, no game.

### Structural identity (established)
The walk's pieces are consecutive landing-index events of a quantized
monotone chain; the right edge of piece k IS the (k+1)-th event. The edge
candidate pre-sorting (attempt 1) is EXACT, but quantization reorders the
sequence relative to any local model; only liberal event evaluation
disambiguates — that is the very bisection we hoped to delete. With the
edge bracket constrained to 4 wei for climb invariance (coarser tolerances
fail adversarial oracle tests, loop-13), the search cost stays at
log2(piece_width) ≈ 55–70 sims/piece.

### Remaining genuine lever (proposal, needs go-ahead)
Cross-block memoization is the only mechanism that can bypass the edge-solve
cost: rewalking identical pool-state compositions per block is the norm (the
path set changes slowly; gates/walkers resist per-block recompute). Add
`walk_run_memo`: pool-state fingerprint per hop (projection's ClCrossingTable
+
profile tables are Arc-shared — hashable identity already available) and
remember each piece sequence per composition per block. Safety =
re-derive on any changed fingerprint; style: telemetry-first (hit-rate,
memory upper bound) behind the existing `DEGENBOT_SOLVER_*` env family,
wired through the same stale-proof captures.

### Verification basis
- 103/103 lib tests green on all candidate variants (oracle families +
  corner test + arithmetic-tie suite).
- live_capture_loop13.jsonl: 104/104 golden match, deterministic 104/104.
- Slowest-path deterministic replay used as the measurement instrument.
