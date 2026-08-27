# CL cache-lab sweep & rebuild matrix (S0-S7)

Task: Z5NOPD (epic KIMRKS) — 369-line corpus sweep of the instrumented CL-table
cache strategies, every solve byte-compared against the full-rebuild reference.

## How the cache layer works (high level)

Reference for reading the matrix. The cache-less reference solve
(`int_solve_cl_path`) rebuilds two derived tables per hop on every solve:

1. **Crossing table** — one entry per range; each entry carries the
   accumulated integer `(gross_input, output)` crossing amounts as the walk
   moves through successive ranges. The active-set Möbius walk consumes it to
   jump across already-crossed ranges. The entries are cumulative, so they can
   be stored as per-range *segments* (diffs) and re-summed cheaply.
2. **Word profiles** (`ClProfileTable`) — one optional profile per range,
   built only for "dense" ranges spanning ≥ `WORD_PROFILE_THRESHOLD`
   256-tick word boundaries. It is a piecewise bucket curve that lets the
   solver and profit envelope skip straight to the next meaningful boundary.

Deriving both from a live sequence is the dominant per-solve cost when
uncached (see `docs/hotpath-crossing-cache-verification.md`).

The seam: every call passes the hop sequences plus a
`CacheEvent` (`Fresh` | `PriceMove` | `Liquidity` | `TickCross` |
`Restore`) to `strategy.refill(seqs, event)`, which returns one
prepared `(crossing, profile)` pair per hop, consumed by
`int_solve_cl_path_cached`. Each strategy answers one question per hop:
*can I serve the solver a correct table cheaper than rebuilding?* Correctness
is exact — every epoch's solution must be byte-identical to the S0
full-rebuild reference; any divergence is a failure, not a tolerance band.

- **Memo** (S1 fused epoch, S5 probe): key = (hop index, full state key). Hit
  → clone two `Arc`s in O(1); miss → rebuild both tables and store. "Fused"
  = crossing table and profile table are invalidated as one unit. S5 adds
  first-sight bookkeeping (`sequence_rebuilds`) on top of the same memo.
- **Patch** (S2 price overlay): on a price-only move with structure unchanged,
  recompute only the new price's effect at range 1 and propagate that delta
  across all subsequent prefix entries; rebuild just range 0's profile.
- **Segment** (S3 prefix, S4 dirty-suffix): store per-range crossing segments
  instead of prefix sums; on `PriceMove` recompute only segment 0 and
  re-sum the table O(n) (S4 extends this to `Liquidity` by resegmenting
  just the jittered range).
- **Split** (S6 profile, S7 composite): cache crossings and profiles at
  different granularities — crossings keyed by full state, profiles keyed
  per-range by ending-range identity — so a price-only move reuses most
  profile tables. S7 combines S4-style segment crossings with S6-style split
  profiles.

**Why S1 wins.** The patch/segment family looks cheaper in theory and leads
the small CI pin (6 price-only rebuilds vs S1's 26), but under the deep
corpus its validity guards trip often — caches carry segments across
different pool states and any shape mismatch forces a full rebuild anyway
(198 price-only rebuilds vs S1's 89; 817 vs 214 total crossing builds). S6
touched profiles 173,019 times vs S1's 214. The fused-epoch memo is the
smallest cache that stays exact on the real stream: deterministic O(1) hit
or clean full rebuild, no partial-state corruption risk.

## Method

- Harness: `rust/crates/degenbot-solvers/examples/cl_cache_lab.rs`, replaying
  `rust/crates/degenbot-solvers/tests/fixtures/heavy_cl_solve_captures.jsonl`
  (420 corpus lines) as deterministic pool-state transition commands.
- Strategies refill crossing tables + word profiles through the same production
  builders and solve through the production `int_solve_cl_path_cached` entry;
  no parallel implementation (measurement is of the real code path).
- Each path's transition stream is seeded from `(path_id, line_index)`; event
  classes are sampled price-only 2/5, liquidity 1/5, tick-cross 1/5,
  restore 1/5.
- Counters: `crossing_tables` / `profile_tables` = tables built,
  `sequence_rebuilds` = full (crossing+profile) hop rebuilds,
  `partial_rebuilds` = segment/profile patches, `solves` = epochs solved.
- Exactness: every strategy's byte-identical output vs the S0 full-rebuild
  reference is required at every epoch. The harness prints the
  total-divergences line only at normal completion and exits 1 on any
  divergence; all runs below completed with 0 divergences and no DIVERGENCE
  lines, so every run exited 0.

## 1. 369-line corpus sweep (the deliverable run)

`DRCLAB_MAX_PATHS=369 DRCLAB_TRANS=2` → 369 paths x 2 transitions = 738 epochs.
Wall clock **9 min 0 s** (file birth 10:25:30Z to final write 10:34:30Z,
including process start). A second capture of the same run is byte-identical
(deterministic transition streams).

Validation-gate invocation:

```
DRCLAB_MAX_PATHS=369 DRCLAB_TRANS=2 cargo run -q -p degenbot-solvers   --manifest-path rust/Cargo.toml --example cl_cache_lab --   rust/crates/degenbot-solvers/tests/fixtures/heavy_cl_solve_captures.jsonl
```

### Table A — class rebuild deltas, 369x2 sweep (crossing tables built)

| Strategy | price-only | liquidity | tick-cross | restore |
|---|---:|---:|---:|---:|
| S0_full_rebuild | 268 | 157 | 155 | 158 |
| S1_fused_epoch | **89** | **60** | **39** | **26** |
| S2_price_overlay_patch | 198 | 210 | 222 | 187 |
| S3_segment_prefix | 198 | 210 | 222 | 187 |
| S4_dirty_suffix_segments | 198 | 121 | 222 | 187 |
| S5_seq_memo_probe | **89** | **60** | **39** | **26** |
| S6_profile_split | **89** | **60** | **39** | **26** |
| S7_composite_split | 198 | 121 | 222 | 187 |

### Table B — cumulative counters, 369x2 sweep

| Strategy | crossing | profiles | seq_rebuilds | partial | solves |
|---|---:|---:|---:|---:|---:|
| S0_full_rebuild | 738 | 738 | 0 | 0 | 738 |
| S1_fused_epoch | 214 | 214 | 214 | 0 | 738 |
| S2_price_overlay_patch | 817 | 817 | 0 | 160 | 738 |
| S3_segment_prefix | 817 | 817 | 0 | 160 | 738 |
| S4_dirty_suffix_segments | 728 | 728 | 0 | 249 | 738 |
| S5_seq_memo_probe | 214 | 214 | 977 | 0 | 738 |
| S6_profile_split | 214 | 173,019 | 0 | 0 | 738 |
| S7_composite_split | 728 | 173,019 | 0 | 249 | 738 |

**Total divergences: 0** (all 8 strategies, all 738 epochs).

## 2. CI pin (12 paths x 6 transitions = 72 epochs)

Authoritative pin data from the pre-sweep run of the first 12 corpus lines.

### Table C — class rebuild deltas, CI pin

| Strategy | price-only | liquidity | tick-cross | restore |
|---|---:|---:|---:|---:|
| S0_full_rebuild | 32 | 16 | 10 | 14 |
| S1_fused_epoch | 26 | 14 | 6 | 3 |
| S2_price_overlay_patch | **6** | 21 | 10 | 20 |
| S3_segment_prefix | **6** | 21 | 10 | 20 |
| S4_dirty_suffix_segments | **6** | 9 | 10 | 20 |
| S5_seq_memo_probe | 26 | 14 | 6 | 3 |
| S6_profile_split | 26 | 14 | 6 | 3 |
| S7_composite_split | **6** | 9 | 10 | 20 |

### Table D — cumulative counters, CI pin

| Strategy | crossing | profiles | seq_rebuilds | partial | solves |
|---|---:|---:|---:|---:|---:|
| S0_full_rebuild | 72 | 72 | 0 | 0 | 72 |
| S1_fused_epoch | 49 | 49 | 49 | 0 | 72 |
| S2_price_overlay_patch | 57 | 57 | 0 | 29 | 72 |
| S3_segment_prefix | 57 | 57 | 0 | 29 | 72 |
| S4_dirty_suffix_segments | 45 | 45 | 0 | 41 | 72 |
| S5_seq_memo_probe | 49 | 49 | 86 | 0 | 72 |
| S6_profile_split | 49 | 9,135 | 0 | 0 | 72 |
| S7_composite_split | 45 | 9,135 | 0 | 41 | 72 |

**Total divergences: 0.**

## 3. Prefix probe (12 paths x 2 transitions = 24 epochs)

The exact first-2-transitions prefix of the CI-pin paths, run as a decomposition
check during this task.

### Table E — class rebuild deltas, 12x2 prefix probe

| Strategy | price-only | liquidity | tick-cross | restore |
|---|---:|---:|---:|---:|
| S0_full_rebuild | 8 | 5 | 4 | 7 |
| S1_fused_epoch | 9 | 6 | 4 | 3 |
| S2_price_overlay_patch | **5** | 7 | 4 | 9 |
| S3_segment_prefix | **5** | 7 | 4 | 9 |
| S4_dirty_suffix_segments | **5** | 6 | 4 | 9 |
| S5_seq_memo_probe | 9 | 6 | 4 | 3 |
| S6_profile_split | 9 | 6 | 4 | 3 |
| S7_composite_split | **5** | 6 | 4 | 9 |

Cumulative counters (12x2): S0 24/24/0/0, S1 22/22/22/0, S2 25/25/0/5,
S3 25/25/0/5, S4 24/24/0/6, S5 22/22/30/0, S6 22/6,697/0/0, S7 24/6,697/0/6 —
each at solves=24. **Total divergences: 0.**

## 4. Reading the numbers

- Internal consistency: in every run, each strategy's class-delta row sums
  exactly to its `crossing_tables` counter (Table A rows sum to Table B
  `crossing`; same for the two pin sets). The lab's accounting is coherent.
- **Corpus-tail decomposition.** Subtracting the 12x2 prefix probe from the
  369x2 sweep locks the remaining 714 epochs (paths 12..368):
  price-only crossing builds there are S1/S5/S6 **80**, S2/S3/S4/S7 **193**,
  S0 **260**. The overlay/segment family wins the pin sets (5-6 price-only
  rebuilds vs 9-26 for the memo family) but degrades ~2.4x deeper into the
  corpus (817 vs 214 total crossing builds), where per-hop caches carrying
  foreign segments across paths make its patch guard fail and rebuild.
- S6_profile_split builds 173,019 profile tables for 214 crossing tables on
  the sweep (and 9,135/6,697 on the pin sets) — a per-solve profile
  construction storm, the dominant cost anywhere it runs.
- S5_seq_memo_probe's class-delta matrix is identical to S1's, but its
  `sequence_rebuilds` counter is 977 vs S1's 214 (and 86 vs 49 on the CI
  pin) — strictly more rebuild work for identical reuse behaviour.

## 5. Recommendation

**Promote S1_fused_epoch.** On the 369-line corpus sweep S1 ties S5 and S6
for the fewest price-only rebuilds (89 vs S0 268 and the overlay/segment
family's 198) with 0 divergences everywhere, and among the tied three S1 has
the lowest total rebuild work: 214 crossing + 214 profile + 214 sequence
rebuilds, versus S5's 977 sequence rebuilds and the 173,019-profile storm of
S6. The same break holds on the CI pin (seq_rebuilds 49 vs 86, profiles 49
vs 9,135), and S1 is the incumbent already landed in production (the
`hp_cl_solve_final2.json` comparator), so promotion carries no new-cache
semantics risk — the fused memo is the robust choice at corpus depth.

## 6. Gaps / next step

- The harness does not emit per-path timestamps, so per-path wall medians/p95
  cannot be reported factually from this data; the sweep wall is 9m0s for
  738 epochs. Latency evidence is the job of the next task's hotpath run
  (KGXFT7) against `logs/hp_cl_solve_final2.json`.
- No fixture or corpus changes: the profit-envelope test still checks only
  the first 12 full goldens as intended.
