# Spike: incremental convex-hull maintenance strategy + microbench

**Status:** done — strategy recommendation, awaiting user approval (task `KEFFUW`).
**Crate:** `rust/crates/degenbot-order-index`
**Implementation landed:** incremental `insert` (splice-based) + a cross-sign helper in
`i256`, validated against full rebuild by `incremental_insert_matches_rebuild`.

## Microbench (release, 1M candidates, K=50, hull=1572)

| Operation | Cost | Role |
|---|---|---|
| full rebuild (batched `extend`, 1M) | **230 ms** | periodic tightening / cold start |
| incremental build (`insert` loop, 1M) | **87 ms** | hydrate on boot |
| per-block `top_k(50)` @1M (O(N log h) reclassify) | **60 ms** | the *current* per-block cost |
| hull-only `best()` | **7.4 µs** | the cheap per-block floor |
| single interior insert | **68 ns** | a result that stays below the hull |
| single above-hull insert | **50 ns** | a result that becomes a new vertex |

Per-mutation cost is ~6 orders of magnitude below a full rebuild, so **incremental hull
maintenance is decisively worth it**; the only remaining per-block cost worth attacking is the
full-N reclassification (60 ms), which is the thing Task 4 makes incremental.

## Key structural facts driving the design

1. **The hull is X-independent.** Hull membership is pure geometry of `(gas, gross)`; `X` (gas
   price) only affects `net = gross - gas*X` for *ranking*, never hull membership. So between
   blocks with no mutations, the hull needs **zero** maintenance no matter how `X` moves.
2. **The hull only changes on mutation**, and single mutations are sub-microsecond (above).
3. **Losslessness survives a superset hull.** The completeness invariant needs a hull that
   dominates every live point (so `upper_bound(p) >= net(p)`). A hull that still contains a
   recently-demoted/deleted vertex is a *superset* — it only *over*-estimates upper bounds
   (under-prunes), which can never wrongly evict a top-K point. So **removals/demotions can be
   deferred** to a periodic rebuild without violating the invariant.
4. **The one lossless-critical operation is "point drifted above the current hull."** A point
   that rises above the hull would otherwise get a stale-low upper bound → wrongful eviction.
   So every mutation must check "is it now above the hull?" and **splice it in immediately** —
   exactly what the incremental `insert` implements (binary search O(log h) + O(k) splice).

## Recommended strategy (for task `GRFRXI`)

- **Mutations:** incremental. `insert` = splice if above, else no-op (done in this spike).
  `update` = splice if the new value pokes above; else keep hull (the point's bracket still
  bounds it). `remove` = if not a hull vertex, no-op; if a hull vertex, **defer** (leave as
  superset) — reconciled at the periodic rebuild.
- **Periodic rebuild:** every `B` blocks (or when hull grows "stale" — e.g. size drifts well past
  the true hull / accumulates too many deferred demotions), run the 230 ms full rebuild to
  tighten the superset hull. `B` tuned so amortized rebuild cost is negligible (230 ms every,
  say, 100 blocks ≈ 2.3 ms/block).
- **Per-block hot reclassify (the 60 ms to eliminate):** a point is hot iff its **bracket edge's
  `max_net(a,X)`, `max_net(b,X)` >= T** where `T = kth_hull_net(X,k)` is computed over the hull
  only (`~µs` at h≈1500). Bucket points by their bracket edge; as `X` drifts, only edges whose
  max-endpoint-net crosses `T` need their bucket re-examined — no full-N scan. Also fold the
  block's mutations into the hot set directly. Result: per-block cost becomes
  `O(µs hull threshold + touched-edges + mutations + bounded top-K over hot)`, not `O(N log h)`.
- **Top-K over hot:** a bounded top-K pass over the (small) hot set — trivial.

Correctness argument: the hull stays a dominating superset at all times (immediate above-insert +
deferred demotion + periodic tightening), so the lossless-split invariant holds; hot reclassify
via hull-edge buckets reproduces the exact `top_k` (proven in the prototype invariant suite).

## What's left for `GRFRXI` (not done here, by design — this is a spike)

- Incremental `update`/`remove` with the defer-demotion policy + a periodic-rebuild trigger.
- The hull-edge-bucket hot set with incremental reclassification as `X` drifts.
- Re-measure the per-block path to confirm it leaves the 60 ms O(N log h) regime.

## Validation

- `incremental_insert_matches_rebuild` (differential, proptest, 2000 cases) + the full invariant
  suite: green.
- `bench_strategies`: table above.
- `cargo clippy --all-targets -- --deny warnings`, `cargo fmt --check`: clean.

## Checkpoint

Approve the strategy (incremental mutations + superset/deferred-removal hull + periodic rebuild +
hull-edge-bucket hot maintenance) so task `GRFRXI` implements it.
