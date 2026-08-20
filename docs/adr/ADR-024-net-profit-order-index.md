# ADR-024: net-profit order index (`degenbot-order-index`)

**Status: accepted.** Records the design of `rust/crates/degenbot-order-index`
(epic `DCABJT`): a convex-hull order index that ranks large sets of path results
by net profit under a per-block gas price. Covers the data model, the
hot/cold-split correctness argument, the Alloy-type seam guard, dynamic
maintenance, and the integration finding that blocks pre-sim wiring.

## Context

A degenbot MEV engine can hold from thousands up to millions of live path
results. Each result is characterized by two *static* dimensions — `gross`
(gross profit, wei) and `gas` (gas used) — and its **net** profit is

```text
net(X) = gross - gas * X,   X = base_fee_next + priority_fee
```

`X` is **one number shared by every result in a block** and fluctuates between
blocks. The engine must repeatedly select "the most net-profitable K results"
(to simulate/submit), iterating in descending net order, while tracking results
across blocks. Sorting all results by net every block is O(N log N) and, at
millions of results, the re-ranking must be cheap and the structure must stay
lossless as results are inserted/updated/removed.

## Decision

### D0 — The index is `(id, gas, gross)` with a per-query `X`.

Each entry stores its opaque `id` plus the two static dimensions `gas`, `gross`.
Net is *computed per query* from the entry and `X`; it is not stored (it changes
with `X`). The API is the [`OrderIndex<Id>`] trait (`insert/remove/update/best/
top_k/top_k_floor/len/is_empty`).

### D1 — Because `net` is linear in `X`, only the argmax is on the hull; the hull's job is a lossless hot/cold split, not "top-K = hull".

A result is a point `(gas, gross)`; `net(X) = gross - gas*X` maximizes over the
**upper-left convex hull** of the point set. Two facts drive the design:

1. **Only the argmax is a hull vertex.** The full top-K is **not** restricted to
   the hull (an interior point can occupy slot 2..K).
2. The hull instead provides an exact **upper bound** on any result's net:
   `upper_bound(p,X) = max(net(a,X), net(b,X))` over the hull edge bracketing
   `p.gas`. With `T = kth_hull_net(X,k)` (the k-th largest hull net), `T <=
   kth_overall_net` (hull ⊆ results), so
   `upper_bound(p,X) < T  =>  net(p,X) < kth_overall  =>  p ∉ top-K`.
   Results provably below the K-th threshold are evicted to the **cold** set
   without ever losing a top-K result; `top_k` exact-sorts only the **hot** set.

### D2 — Alloy types with a seam guard; no hand-rolled wide integer.

`gross`/`gas`/`X` are `alloy_primitives::U256`; `net` and the hull cross product
are `I256` (Alloy `Signed`, `Ord`). Exactness requires the seam guard `gross <=
2^127`, `gas <= 2^120`, `X <= 2^120`: then every difference and every cross
product (`<= 2^247 < 2^256`) fits `I256` with no overflow and no custom wide
math. Realistic magnitudes are orders of magnitude inside the guard. Enforced by
`clamp_gas`/`clamp_gross` at the seam.

### D3 — Two swappable implementations behind a feature flag.

`OrderIndex` has a brute-force reference `ScanTopK` (always compiled, O(N) per
query) and the convex-hull `EnvelopeIndex` (feature `envelope`, default on). A
shared proptest invariant suite drives **both** (top-K == brute force, `best` is
a maximizer) over randomized points/`X`, plus differential insert/update/remove
sequences. This gives RED→GREEN proof of the lossless split and a redundant
reference for any change.

### D4 — Dynamic maintenance: the hull is X-independent; mutations are incremental via a snapshot hull.

The hull is pure geometry of `(gas, gross)` and does not depend on `X`, so it only
changes on mutation. `EnvelopeIndex` holds the hull as a **snapshot** (values,
independent of live-point indices), which makes mutations cheap:
- `insert` / non-hull `update`: splice into the hull *only if the point pokes
  above it* (lossless-critical), else no-op — O(log h);
- non-hull `remove`: O(1) (the snapshot hull still dominates everything);
- **hull-vertex** `update`/`remove`: full rebuild (correct, bounded by the small
  frontier churn).
Measured at 1M results: build ~270ms, non-vertex update+remove ~199ns each,
per-block `top_k(50)` ~87ms, single insert ~150-200ns. The 87ms `top_k` is an
O(N log h) rescan; eliminating it (incremental hot reclassification) needs a
gas-ordered index and is separately tracked (not on the critical path).

### D5 — Housekeeping is per-block-floor and app-tiered.

The per-block profit floor is `top_k_floor(X,k,min_net)` (evaluated against the
block's `X`, not a static wei floor); the cold set is exactly the envelope's
non-hot set (`hot_len`). Multi-tier containers (hot/warm/cold), expiry/freshness
TTL and idle cold re-evaluation are application housekeeping layered on the
index, not in-crate structure.

## Integration finding (why end-to-end selection is deferred)

The live bot's pre-sim selection seam (`dispatch_profitable_results`, step 4) has
**no per-item gas** — `SolvePathResult` carries `profit`/`optimal_input`/
`consumed_inputs` but gas is only known *after* simulation. So `net = gross -
gas*X` cannot be computed pre-sim, and the pre-sim ordering is profit-only (which
degenerates the hull to a plain sort and needs no convex hull). End-to-end
OrderIndex selection in the bot is therefore **blocked on the solver exposing
per-path gas**, plus a settled (non-concurrently-modified) bot codebase. The
crate-level parity of the index itself is proven by the invariant suite and the
scale demo.

## Addendum — A33CRA (2026-08-20): gas-ordered hot-range reclassification

The deferred gas-ordered index (canceled 4Y6763's recorded path) has landed.
The per-block `top_k` no longer rescans all N points with a per-point hull
search. Key lemma (documented in `envelope.rs`): because `upper_bound` depends
on a point only through the hull edge **bracketing its gas**, the hot set is
the single gas interval `(v_{L-1}.gas, v_{R+1}.gas)` where `v_{L..R}` are the
hull vertices with `net(v_i, X) >= T` (hull vertex nets are unimodal at fixed
`X`, so the block is contiguous; the min/max scan is exact even without that).
Live points are mirrored in a `BTreeMap<U256 /*gas*/, Vec<Id>>` (O(log N) per
mutation, one touched bucket), and per-block reclassification is one range
walk — O(h + log N + P_hot log k + k log k) instead of O(N log h) — with the
final k-heap rank bounded by `k`. The lossless superset invariant is
unchanged: the range reproduces the old per-point `upper_bound >= T` filter
bit-for-bit.

Measured (`bench_strategies`, K=50): per-block `top_k` 80.2ms -> 21.9ms @1M
(hot=51,579), 145ms @10M (hot=245,973) — cost now proportional to the hot set,
not N. Incremental insert pays +~30-160ns for the bucket entry (169ns -> 198ns
interior; 190ns -> 349ns above-hull). Differential coverage added:
`mutation_plus_x_sequence_matches_scan_topk` (mutations interleaved with X
sequences vs the brute `ScanTopK` reference).

## Consequences

- Deterministic, lossless top-K selection that scales to millions of results with
  O(1)-ish mutation and a small hot set.
- The `OrderIndex` trait + `ScanTopK` reference mean the envelope's correctness
  is always independently checkable.
- Wiring into the bot is a documented follow-up gated on gas-per-result data.
