# WalkMemo dirty-set overlap: what the capture proves, and the smallest tap that answers why

**Status:** investigation only; no solver or telemetry implementation changed.

This is a follow-up to
[`walkmemo-cross-block-reuse-readout.md`](walkmemo-cross-block-reuse-readout.md).
It reexamines the reported **591,965 distinct compositions over 593,523 probes
(99.74%)** and asks which shares come from admission draw, event-driven
dirtiness, and state/composition novelty.

## Executive findings

1. **99.74% is a within-epoch uniqueness ratio, not a cross-block novelty
   ratio.** `distinct` is the current epoch's `HashSet<u128>::len()`. The clean
   adjacent-epoch composition-recurrence figure is `hits = 2,634 / 593,523 =
   0.444%`.
2. **The reported 1.04% "combined cross-block recurrence" must not be used as a
   unique-probe share.** `negatives_played` includes same-epoch cache replays
   and can overlap `hits`. The static fixture demonstrates the semantics:
   epoch 1 has an empty previous census but 47 negative plays.
3. **Admission and composition novelty are structurally linked.** A path is
   admitted because a pool it contains was written; the memo fingerprint folds
   that newly written state. The admission mechanism therefore selects against
   exact composition recurrence by construction.
4. **An idle cycle can erase the reuse window.** The memo advances before the
   shed/empty-fan-out exits. `begin_block` swaps `prev ← curr` even when the
   cycle produces no probes, so the next active epoch can see an empty previous
   census. The existing zero-activity JSONL gate cannot distinguish this from
   no engine cycle at all.
5. **The retained aggregate data cannot assign causal shares.** It has no path
   id, dirty-pool key, draw census, or per-probe negative-replay disposition.
   The original JSONL and console log are also absent in the current workspace,
   so only the published aggregates are available.

The right next step is not another retention mechanism. It is a small,
opt-in, identity-bearing overlap tap that emits aggregate counters only.

## 1. What the current numbers mean

### 1.1 Within-epoch uniqueness

`WalkMemoState::curr` is a set of content fingerprints. `drain_epoch` reports
`distinct = curr.len()`, so for one epoch:

```text
within-epoch duplicate probes = probes - distinct
```

Using the published totals:

| Measure | Value | Meaning |
|---|---:|---|
| Probes | 593,523 | All memo probes across 862 reported epochs |
| Distinct | 591,965 | Sum of unique fingerprints within each epoch |
| Duplicate mass | 1,558 (0.263%) | Probes repeated within their own epoch |
| Distinct / probes | 99.737% | Within-epoch uniqueness |

This says almost nothing repeats **inside** a block. It does not say 99.74% of
compositions are new relative to the previous block.

There are two aggregation steps, and they must not be conflated:

1. For double-flush epochs, use the final line's `distinct` gauge
   (last-value-wins), not partial + final.
2. Only then sum the per-epoch distinct counts if a whole-run within-epoch
   ratio is wanted.

The readout documents the first rule. The raw slice is absent, so this
investigation cannot independently recompute its 591,965 total or demonstrate
an aggregation error. Any future script must print both the per-epoch and
whole-run calculations rather than summing every line blindly.

Sources: `degenbot-pools/src/solve/concentrated_liquidity/memo.rs`,
`WalkMemoState::drain_epoch`; `degenbot-bot/src/arb_engine/walk_telemetry.rs`,
line-semantics contract.

### 1.2 True previous-epoch recurrence

`hits` is computed by `prev.contains(fp)`. `prev` changes only on a real epoch
advance, so it is an unambiguous previous-census membership test:

```text
previous-composition hit rate = 2,634 / 593,523 = 0.444%
```

This is the only existing scalar that directly measures adjacent-epoch
composition recurrence.

`negatives_played` is different. `consult_cache` runs after the census insert
and counts every served cached negative, whether the fingerprint came from the
previous epoch or was stored earlier in the current epoch. A negative play can
also coincide with a previous-census hit. Therefore:

- `hits + negatives_played` is not a disjoint count;
- `negatives_played` alone is not a cross-block count; and
- the published 1.04% sum should be retired from causal analysis.

The aggregate duplicate mass gives only a broad bound for previous-epoch
negative plays, assuming every within-epoch duplicate could be negative:

```text
3,543 - 1,558 = 1,985 <= previous-epoch negative plays <= 3,543
```

The tap below replaces that bound with two exact counters.

Sources: `memo.rs`, `WalkMemoState::probe` and `consult_cache`; the readout's
static Arm B epoch 1, where `hits = 0` and `negatives_played = 47`.

### 1.3 A uniform-draw baseline, not an attribution

The capture reports 1,000,000 registered paths and median 675 probes per epoch.
A deliberately simple uniform-independent path draw would imply:

```text
path recurrence per probe = 675 / 1,000,000 = 0.0675%
expected shared paths per median epoch = 675 × 675 / 1,000,000 = 0.456
```

The observed previous-composition hit rate of 0.444% is **6.6×** the first
baseline rate. This is only an illustrative comparison: composition identity
is harder to repeat than path identity, probe counts vary, admission is not
uniform, and idle epochs alter adjacency. It suggests structured recurrence,
but cannot quantify admission versus event effects.

## 2. The mechanism producing novelty

The production chain is:

1. **Applied event → `AffectedKey{hop, pool_id}`.** Buffered, refused, and
   no-op applications do not create a solve key.
2. **Ledger promotion.** `EpochDelta::record` keeps one entry per key and moves
   a retouched key into the newest block bucket.
3. **Admission draw.** `draw_freshest` takes newest buckets first, with
   `budget = admission_target_depth - in_flight`; overflow remains pending.
   Defaults are eight keys and a 50-block retention window. A zero budget sheds
   the cycle.
4. **Reverse-index fan-out.** `scan_dirty_status` maps each key through
   `pool_to_paths` and `PathSolveStatus::on_pool_dirty` to the affected path
   set.
5. **Resolve and solve.** A path resolves when its hop clocks moved, then the
   solve entry computes `walk_views_fingerprint` over the current hop state.
6. **Memo probe.** The fingerprint includes liquidity, prices, fee/gamma,
   direction, word-boundary prices, and composed capacity fields.

Sources, in that order:

- `degenbot-substrate/src/log_dispatcher.rs`, affected-key recording
- `degenbot-substrate/src/epoch_delta.rs`, `record`, `draw_freshest`
- `degenbot-bot/src/arb_engine/solve_cycle/shed.rs`, `draw`
- `degenbot-bot/src/arb_engine/solve_cycle/resolve.rs`, `scan_dirty_status`
- `degenbot-bot/src/arb_engine/lane_walk.rs`, `solve_one_path`
- `degenbot-pools/src/solve/concentrated_liquidity/memo.rs`, fingerprint folds

Two conclusions follow directly.

### Admission selects for changed composition

A path enters the probe set because at least one of its pools changed. Its
fingerprint is computed after that change. Same-path recurrence and
same-composition recurrence are consequently different questions: an active
pool can produce a predictable route set while producing almost no identical
state fingerprints.

The eight-key budget also means probe count is not a registry sample size. It
is the fan-out of the selected keys:

```text
probes ≈ Σ over drawn keys (paths activated by that key)
```

A median of 675 is compatible with eight high-fan-out keys and with many
low-fan-out keys. The current JSONL cannot distinguish those cases or say when
backpressure truncated the draw.

### Quiet cycles consume the one-epoch window

`run_epoch` calls `scope.advance(solve_block)` before the zero-budget shed and
empty-fan-out returns. `WalkMemo::begin_block` drains and swaps `prev ← curr`
before those returns. Thus:

- an active epoch followed by a zero-probe cycle leaves the next epoch with an
  empty previous census;
- an inactive chain slot with no engine cycle does not advance the memo; and
- the current zero-activity emission gate records neither case.

Any recurrence rate quoted without cycle-presence data has an ambiguous
denominator. This is a first-order candidate explanation for low hit rates and
must be measured before changing cache retention.

Source: `degenbot-bot/src/arb_engine/solve_cycle/mod.rs`, `run_epoch`;
`memo.rs`, `begin_block`.

## 3. What the existing capture cannot decompose

No current JSONL field identifies:

- which `path_id` produced a probe;
- which `AffectedKey` or keys activated that path;
- whether a path or dirty key appeared in the previous epoch;
- whether a negative play was previous-epoch or same-epoch;
- the budget, drawn/pending/expired key counts, or fan-out per key;
- whether a quiet slot contained a shed/empty cycle or no cycle; or
- whether a composition was absent last epoch but present two epochs ago.

The solve outcome also has no memo-probe disposition, and the existing
`WalkTelemetry` tally is not probe-scoped. These missing identities are why the
99.74% figure cannot be assigned to admission, dirtiness, or state novelty.

## 4. Minimal overlap tap

Add an **opt-in overlap-observation sidecar** alongside `WalkMemo`. Do not emit
per-probe JSON records and do not change solver decisions. The sidecar needs
three epochs of small sets and aggregate counters, not a million-path registry
mirror.

### 4.1 Identities retained per epoch

```text
previous_paths:       HashSet<path_id>
previous_dirty_keys:  HashSet<AffectedKey>
previous_fps:         existing WalkMemo previous census
previous2_fps:        HashSet<u128>  // optional two-epoch comparison

selection[path_id]:   SmallVec<AffectedKey>  // keys that activated this path
```

`selection` is built in `scan_dirty_status`, where both the key and path id are
already present. The previous sets rotate at the same boundary as the memo.
The optional `previous2_fps` distinguishes "not previous" from "present two
epochs ago"; it does not claim a composition was never seen.

### 4.2 Per-probe classification

At the existing memo probe, supply the `path_id` through the same internal
telemetry context that already reaches `solve_one_path`. Do not recompute the
fingerprint. Classify each probe on three independent axes:

| Axis | Values | Meaning |
|---|---|---|
| Path | `New`, `Repeat` | Was `path_id` probed in the previous epoch? `New` does not mean newly registered. |
| Dirty link | `New`, `Repeat`, `Unknown` | Did any key linked to this probe occur in the previous draw? `Unknown` means no direct selection link. |
| Fingerprint | `New`, `Repeat` | Does `prev` contain the fingerprint? `Repeat` is exactly today's `hits`. |

Emit the full **2 × 3 × 2 = 12-cell matrix**. It prevents a hierarchy from
hiding joint causes. The most useful readings are:

| Cell | Interpretation |
|---|---|
| Path New + Fingerprint New | Admission-path novelty |
| Path Repeat + Dirty Repeat + Fingerprint New | Same route and dirty pool, but changed state: composition novelty |
| Path Repeat + Dirty New + Fingerprint New | Same route selected by a different dirty pool/event |
| Path New + Dirty Repeat + Fingerprint New | Same dirty pool, different path fan-out |
| Any Dirty + Fingerprint Repeat | Actual previous-composition recurrence |
| Dirty Unknown | Unclassified; never silently assigned to novelty |

Add one optional counter for probes whose fingerprint is absent from `prev`
but present in `previous2_fps`. This measures gapped recurrence separately
from the immediate one-epoch window.

### 4.3 Negative replay disposition

Inside `consult_cache`, split the existing counter:

```text
negative_previous + negative_current == negatives_played
```

`negative_previous` uses the same `hit_now` fact as `hits`;
`negative_current` is a same-epoch replay. Do not add either to `hits` when
reporting unique previous-composition recurrence.

### 4.4 Per-cycle draw census

Add these scalars to the same boundary record:

- `cycle_observed`, `shed`, `empty_fanout`;
- `budget`, `keys_drawn`, `keys_pending`, `keys_expired`;
- `dirty_keys_repeated`;
- `paths_affected`, `paths_resolved`, `paths_staged`, `probes`;
- `registered_paths`, registry revision if available.

`keys_drawn` is an `AffectedKey` count, not a path count. Name it explicitly;
`CycleArm::Shed.keys_affected` currently receives a path count and must not be
reused as the key count.

When overlap observation is enabled, emit a record for an observed cycle even
when `probes = 0`. Otherwise quiet cycles remain indistinguishable from no
cycle. The existing zero-activity behavior stays unchanged when the new tap is
off.

## 5. Cost and cardinality

The tap adds no per-probe persistence:

- per probe: a few boolean derivations and counter increments under the memo's
  existing lock, plus path/dirty set lookups;
- per cycle: one boundary JSON line with scalar fields;
- memory: `O(unique paths + selection links + distinct fingerprints)` for the
  current and previous epochs, plus one optional fingerprint set for
  `previous2_fps`.

The working set is therefore proportional to actual probes and selection
links—median hundreds per epoch in the capture—not to the one million
registered paths. `previous2_fps` costs one additional epoch of `u128` entries.
A path with many activating keys increases the selection-link count, so the
capture should report that count and its maximum per path.

The memo's fingerprint, cache, eviction, and solve behavior remain untouched.
The feature is observation-only and default-off.

## 6. Fail-closed analysis gate

A capture reader must stop rather than infer shares when these checks fail:

1. **Schema.** Every record has the overlap fields. The existing capture fails
   this check and may support only Sections 1–2, never an attribution.
2. **Partition.** The 12 cells sum exactly to `probes`; cells with fingerprint
   `Repeat` sum exactly to `hits`.
3. **Negative disposition.** `negative_previous + negative_current ==
   negatives_played`; `negative_previous <= hits`.
4. **Aggregation.** Double-flush counters sum, while `distinct` is
   last-value-wins per epoch before any whole-run ratio is computed. Reorg-held
   lines are excluded from ordinary adjacent-epoch claims.
5. **Draw consistency.** `keys_drawn <= budget`, and path-stage counts satisfy
   `paths_affected >= paths_resolved >= paths_staged`. Do not assume unique
   staged paths equal probes: a path may be refused before probing or probed
   more than once. Unknown dirty links are reported, not forced into `New`.
6. **Cycle denominator.** A hit-rate denominator includes only epochs whose
   immediately preceding memo epoch is known and whose predecessor's probe
   count is visible. Quiet-cycle/no-cycle ambiguity suppresses a rate.
7. **Coverage.** Quote shares only over a declared consecutive run with enough
   active cycles and complete selection links. Preserve the raw JSONL and
   console log with the analysis.

These are analysis-time checks for a runtime artifact, not a permanent CI
tripwire.

## 7. What the next capture would decide

| Dominant evidence | Conclusion | Appropriate response |
|---|---|---|
| Path New + Fingerprint New | Admission keeps presenting different routes | Inspect reverse-index fan-out, key-bucket rotation, and registry shape—not cache retention |
| Dirty Repeat + Path New + Fingerprint New | The same pools activate different route sets | Inspect paths-per-key and dedup/signature policy |
| Path Repeat + Dirty Repeat + Fingerprint New | Re-admitted routes change state every time | Event timing and co-hop activation dominate; memo revision counters will not help |
| Fingerprint absent previous but present previous2 | The one-epoch window is too short for gapped recurrence | Reopen a bounded multi-epoch census evaluation |
| Many zero-probe predecessor cycles | The effective reuse window is being erased | Revisit when census advancement occurs, before changing retention size |
| Dirty Unknown large | The tap lacks causation for detached/pending work | Add the missing selection provenance before drawing conclusions |

Until that capture exists, the defensible conclusion is limited: within-epoch
composition duplication is rare (0.263%), previous-epoch fingerprint hits are
0.444%, and admission-by-state-change plus census-erasing quiet cycles are
strong structural candidates. Their individual shares are unknown.

## Recommendation

Keep the original per-pool revision and revision-keyed cache work shelved.
Implement no solver behavior change from this investigation. Run the existing
multi-block capture again with the opt-in overlap tap, preserve its raw
artifacts, and evaluate the 12-cell matrix and idle-predecessor counts first.

That is the smallest experiment that can answer the task's three-way
decomposition without inventing causal shares from aggregate counters.
