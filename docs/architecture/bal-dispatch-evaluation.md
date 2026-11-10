# BAL carry-forward dispatch evaluation

Closing evaluation for the parent-linked block-access-list (BAL) carry-forward
dispatch epic: what the committed implementation does, what the evidence proves,
what it does not, and the operator recommendation.

Evidence base, all re-read at `cfaf61199`:

| artifact | kind | what it prices |
| --- | --- | --- |
| `logs/bal_dispatch_scaling_result.md` | controlled probe, one exact head, one candidate list, scripted chain (`just bal-dispatch-scaling-probe`) | the dispatch mechanism and its width scaling |
| `logs/bal_live_ab_result.md` | live OFF/ON window pair, first revision (`just bal-live-ab-report`) | readiness of the first shipped pipeline |
| `logs/bal_live_ab_v2_result.md` | live OFF/ON window pair, after canonical-head preparation | readiness, per-head background walls, first identity counts |
| `logs/bal_live_ab_v3_result.md` | live OFF/ON window pair, after the idle-window policy | readiness, per-head background walls, held/live identity counts |
| `logs/bal_retention_result.md`, `logs/bal_delta_rounds_result.md` | replay and retention probes (`just bal-retention-probe`, `just bal-delta-rounds-probe`) | the retention policy's cost and benefit |

`logs/` is gitignored, so those files are local evidence rather than tracked
artifacts. Every figure quoted below was read back from them; no figure in this
document is new. "Factual uncertainty in this audit" lists what could not be
verified and where two sources disagree.

## Verdict

**GO for an opt-in production soak. NOT default ON.**

| claim | status | where it is shown |
| --- | --- | --- |
| The carry is value-correct: every served word equals an independent cold read, at every anchor | proven (replay over both committed corpora; a planted one-wei divergence is caught) | `bal_retention_result.md`, `bal_delta_rounds_result.md` |
| Dispatch readiness after canonical-head preparation | proven at window scale (26 of 27 heads carried; 6 of 7 and 10 of 11 fenced fan-outs on `delta`) | `bal_live_ab_v2_result.md`, `bal_live_ab_v3_result.md` |
| The mechanism removes real per-fan-out storage work | proven on one exact head: one prefetch batch (11..403 members) plus one serial read deleted per fan-out, verdicts unchanged | `bal_dispatch_scaling_result.md` |
| Retention bounds an entry's idle lifetime, never the store's size | proven as a LIFETIME bound only (fixed eight-block idle window, memory released at compaction folds); size tracks the eight-block active set plus the fold horizon and carries no absolute cap | `bal_live_ab_v3_result.md`, `bal_retention_result.md` |
| An absolute entry or byte cap on the store | **absent, and not implied by the window** — a continually novel active read/write working set keeps seeding entries, and identity counts are not memory | "Retention: an idle-lifetime bound", "Limits" |
| Long-run RSS is bounded | **unproven** — no capture reports bytes or RSS | "Limits" |
| A live market-scale latency benefit | **unproven** — the live windows are disjoint, non-paired and lossy; the controlled win is synthetic | "Limits" |

The unproven half is exactly what a soak settles, and the asymmetry between the
proven and the unproven halves is the whole recommendation. Measuring the
unproven half is mostly a matter of reading rows the opt-in already emits, but
not entirely. The `bal-head` row reports prepare median/p90/max rather than p95;
the per-fan-out `cands=` line's cold-storage counters aggregate every cold read
the fan-out paid, so a fallback's own serial read is not separable there; and no
row reports bytes or RSS at all. A soak may therefore need a p95 field, a
read-origin counter or an RSS sampler, and the acceptance criteria below name the
measurement each one requires instead of assuming the existing telemetry carries
it. The verdict is unchanged: promote nothing, soak the opt-in, and gate
default-ON on those criteria.

## The mechanism, in operator terms

One flag, `simulation.delta_dispatch` (`DEGENBOT_DELTA_DISPATCH`), default
`false`, read ONCE at construction by each warm-cache owner
(`rust/crates/foundation/degenbot-config/src/schema.rs:645`,
`rust/crates/engine/degenbot-strategy/src/market_context.rs:165`). There is no
retention knob, no readiness knob and no second switch. Operator-facing
documentation: `docs/rust-config-keys.md`.

OFF is the shipped posture and requests nothing at all: the session installs no
pipeline, no canonical-head sink and no driver, so no code path exists that could
issue a BAL request (`WarmCodeCacheInner::shared_default`,
`rust/crates/engine/degenbot-simulation/src/sim/evm2/warm_code_cache.rs:383`;
sink installation, flag-gated, in
`rust/crates/shells/degenbot-python/src/bot/engine/register.rs:90`).

ON adds one session-scoped pipeline on the block pump's accepted-header edge:

```text
accepted canonical head (number, hash, parent_hash)   ← the pump already has it
     │ BalHeadPipeline::on_canonical_head              ← synchronous, no RPC
     ▼
deliver(head) → ticket                                ← the head is addressable
     │ spawn                                           ← never awaited by the pump
     ▼
prepare(ticket)  = BAL fetch by exact hash + full decode
     ▼
relevance        = the SESSION's set, read after the fetch, before the apply
     ▼
apply(ticket)    = final writes installed in the new generation
```

Four consequences an operator can hold onto:

- **A BAL is requested per accepted head, not per fan-out.** Candidate-bearing
  blocks are structurally sparse (27 of 28 and 27 of 27 in the v2 arms, 27 of 38
  in v3's ON arm), so a candidate-driven clock can almost never be consecutive.
  Coverage is 100 % of accepted heads instead
  (`rust/crates/engine/degenbot-simulation/src/sim/evm2/bal_heads.rs`).
- **Dispatch never generates BAL work — it only asks.** A fan-out resolves its
  anchor's exact `(number, hash)` in one header read and queries the session's
  one driver (`warm_code_cache.rs:260`, `select_dispatch_storage`). A ready
  generation answers; anything else takes the shipped legacy lane with a
  closed-set reason.
- **The read window is two consecutive exact generations** — the installed head
  and the one it replaced — because a fan-out anchors at
  `max(candidate.solve_block)`, one block behind the newest header (occasionally
  two). Two-or-more lag is cold. The previous slot is set only when the
  transition actually linked (`previous + 1` with a matching parent hash), so a
  gap makes the window smaller, never wrong.
- **Relevance gates seeding only.** A write to a key the generation still serves
  overwrites it whatever relevance says, a zero or deletion final included; a
  write to an unknown key seeds only when its address is in the session's set
  (`seed_final_writes`,
  `rust/crates/engine/degenbot-simulation/src/sim/evm2/delta_snapshot.rs:661`).
  The set is derived once per registry revision, so a steady session pays nothing
  per head.

Readiness, retention and endpoint facts:

- **A fan-out at the exact in-flight head waits for that head's terminal
  result** (post-evaluation change, after `cfaf61199`). The same-block
  readiness wait this evaluation's limit 9 called future behaviour has since
  landed: the dispatch selection joins the head's EXISTING preparation through
  the driver's per-head completion notification and re-reads the exact anchor
  after the settle. The wait ends on real outcomes — a settle (carried or cold,
  a provider failure included), a supersession, or the preparation's
  abandonment — never on a timer, and a generation no live preparation owns
  resolves terminal `abandoned` at that exact anchor instead of parking. The
  fallback labels are `abandoned`, `superseded`, `undelivered`,
  `lineage-refused`, `bal-unreadable`, `settled`, `lineage-unreadable` —
  `abandoned` means the head's preparation did not reach a BAL verdict and the
  fan-out used the legacy lane — and a fan-out one block behind the head still
  finds its anchor ready through the read window.
- **Retention is a fixed eight-block idle window**
  (`DEVELOPMENT_IDLE_BLOCKS = 8`, `warm_code_cache.rs:145`) at the default fold
  depth. An entry no candidate has READ for eight blocks stops being served; a
  candidate read refreshes that clock, a BAL overwrite never does; a read of an
  expired entry takes one dynamic read at the fan-out's own anchor and settles a
  fresh window. Expiry is invisibility, never staleness. Memory is released at
  the next compaction fold, so `held` falls at the fold horizon, not at the
  instant of expiry. The window bounds an entry's IDLE LIFETIME, not the store's
  size: a working set that keeps naming identities no candidate read inside
  eight blocks keeps seeding, so `held` tracks the eight-block active set plus
  the fold horizon and nothing caps it.
- **The BAL source is either official EIP-7928 endpoint.** The raw
  `debug_getRawBlockAccessList` RLP representation is asked first and, when the
  node does not serve that method (`-32601`/`-32004`, or a refusal whose message
  names the method itself as absent — never one about the block or the state),
  the acquisition falls back to the official `eth_getBlockAccessList` JSON one at
  the same exact block hash; the classification is sticky on the provider, so an
  eth-only node pays one refused raw request per session rather than one per
  head. Both representations decode through one codec into the same `BalSeed`
  (final writes, zeros included, and read-only identities), so nothing below the
  acquisition can tell which answered. Every other failure — a transport error, a
  `null` answer, a payload that is not one complete list — is closed: it yields
  `bal-unreadable` and the legacy lane, never a second request and never a wrong
  value. An endpoint serving neither representation is the same closed failure.
- **The lane is visible in telemetry.** Every opt-in fan-out reports
  `degenbot.dispatch.storage{mode,reason}` (`mode` = `delta` | `legacy`; `reason`
  = `none` or the closed cold set), and a ready `delta` fan-out leaves
  `degenbot.storage.prefetch` untouched because it dispatches no batch. One
  `bal-head` diagnostic row per head transition carries the exact refusal or BAL
  skip, the prepare/apply walls, and the generation's `held`/`live`/`expired`
  identity counts.

## Live A/B evidence

Three window pairs, each two live runs on the same host with the same
owner-patched executor and the same configuration apart from the flag. Every
count is a lower bound (the console sink drops and counts records), and the
windows are disjoint in block range, so no candidate is paired across arms.

| fact | v1 | v2 | v3 |
| --- | --- | --- | --- |
| OFF capture | `logs/bal_dispatch_off.log` | `logs/bal_dispatch_v2_off.log` | `logs/bal_dispatch_v2_off.log` |
| ON capture | `logs/bal_dispatch_on.log` | `logs/bal_dispatch_v2_on.log` | `logs/bal_dispatch_v3_on.log` |
| ON block range | 26382797..26382832 | 26383570..26383596 | 26384023..26384060 |
| fenced fan-outs | 5 | 7 | 11 |
| `delta` / `legacy` fan-outs | 1 / 4 | 6 / 1 | 10 / 1 |
| legacy fallback reasons | `lineage-refused`=4 | `undelivered`=1 | `undelivered`=1 |
| `bal-head` rows | 0 | 27 | 38 |
| settled / cold heads | n/a | 26 / 1 | 37 / 1 |
| prepare: heads / total ms / median / p90 / max | n/a | 27 / 4110.76 / 148.33 / 242.94 / 524.38 | 38 / 4364.45 / 94.63 / 214.96 / 320.03 |
| apply: total ms / median / max | n/a | 2.29 / 0.08 / 0.20 | 2.90 / 0.07 / 0.17 |
| chain size at the last head | n/a | 6420 (`carried`) | 2912 (`held`) |
| held / live / expired at the last head | n/a | not reported | 2912 / 2173 / 739 |
| ON re-read gap p99 / inside 8 blocks | 17 / 96.61% | 6 / 99.75% | 16 / 98.23% |
| OFF re-read gap p99 / inside 8 blocks | 9 / 98.97% | 8 / 99.25% | 8 / 99.25% (same capture) |

Prepare and apply are the head-arrival pipeline's BACKGROUND walls: the
transition that settles a head pays them on whichever task performs it, and they
are never part of a fan-out's `fanout_ms`. They are not netted out of any
dispatch wall.

### v1: the sparse-head defect, and what fixed it

v1 resolved 1 of 5 fenced fan-outs as `delta`; the other four fell back with
`lineage-refused`. The committed block/anchor pairs determine each refusal
uniquely: the first delivery had no parent, and the other three followed an
anchor whose installed generation was not `anchor + 1` with a matching parent
hash. The single success is the single case where two candidate-bearing heads
happened to be consecutive. The chain was never at fault; the delivery clock was,
because it was driven by candidates on blocks that are structurally sparse.

The fix is the canonical-head pipeline: every accepted head is delivered,
prepared and applied off the dispatch critical path, so coverage stops depending
on whether a candidate arrived.

### v2 and v3: readiness at window scale

| revision | carried heads | fenced fan-outs on `delta` | the fallbacks |
| --- | --- | --- | --- |
| v1 | no head rows | 1 / 5 | four `lineage-refused` |
| v2 | 26 / 27 | 6 / 7 | one `undelivered` (the first fenced fan-out, before any head was delivered) |
| v3 | 37 settled, 1 cold of 38 | 10 / 11 | one `undelivered` (the same boot position) |

Both later windows' single fallback sits at the boot edge, where the fan-out
precedes the first delivered head. Nothing in the three windows shows a settled
head failing to serve its own anchor, and every ready `delta` fan-out reports no
prefetch row while every fallback keeps a full batch.

The prepare wall improved between v2 and v3 (median 148.33 → 94.63 ms, p90
242.94 → 214.96 ms, max 524.38 → 320.03 ms) but that pair is confounded:
different head ranges, different write volumes, and v3 runs under the idle
window. It is a direction, not a measured retention saving on the prepare path.

### What the live windows still cannot price

- The windows are **not paired**: disjoint block ranges, different candidate
  mixes (v2 shares 76 candidate ids with its OFF arm, v3 shares 0), different
  readiness mixes.
- The capture is **lossy**: every count is a lower bound, and a fan-out whose
  selection row survived can be missing its prefetch and `cands=` rows.
- Per-lane candidate timing is **mostly not recoverable**: the trace stream is
  shared with the inline hook re-quote arm, so only the per-fan-out dispatch rows
  name a lane, and each fenced fan-out carries one or two observations.
- The small-count cross-lane comparison is a **direction, not a speedup**: at
  counts 2 and 4 (v2) and 1, 2, 4 and 5 (v3) the ON `delta` fan-outs paid no more
  `fanout_ms` than the OFF `legacy` ones, with one tie at v3's count 2 (legacy 4
  ms against delta 4 ms), over one or two fan-outs per arm at millisecond
  resolution, at different heads.
- The largest live fan-out (50 candidates) appears only on the ON arm, so
  burst-scale dispatch has no count-matched live counterpart.
- No window reports bytes or RSS.

## Controlled dispatch scaling on one head

`logs/bal_dispatch_scaling_result.md` is the measurement the live windows said
was missing: ONE exact head, ONE candidate list, ONE deployment declaration, ONE
scripted provider state, six legs per point (OFF legacy, ready-current,
ready-previous, ready-unknown, cold-failed, cold-undelivered), 3 interleaved
repetitions, driven through the production entry
(`dispatch_profitable_results`). The storage representation is the only
variable. Every simulated candidate executes a real SLOAD of the executor's own
slot `0x5eed` and the receipt is a function of the stored word, so the fixture is
value-sensitive; every point runs both a zero and a nonzero word.

Ready versus legacy on the same head, median wall:

| candidates | word | latency model | legacy med us | ready-current med us | delta | prefetch members | serial reads | modeled us legacy / ready |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | zero | 0 us | 51 | 30 | -41.2% | 11 | 1 / 0 | 0 / 0 |
| 1 | zero | 400 us | 2217 | 44 | -98.0% | 11 | 1 / 0 | 1020 / 0 |
| 16 | zero | 0 us | 391 | 133 | -66.0% | 131 | 1 / 0 | 0 / 0 |
| 16 | zero | 400 us | 5636 | 178 | -96.8% | 131 | 1 / 0 | 3420 / 0 |
| 50 | zero | 0 us | 1077 | 357 | -66.9% | 403 | 1 / 0 | 0 / 0 |
| 50 | zero | 400 us | 12498 | 477 | -96.2% | 403 | 1 / 0 | 8860 / 0 |
| 50 | nonzero | 0 us | 1145 | 434 | -62.1% | 403 | 1 / 0 | 0 / 0 |
| 50 | nonzero | 400 us | 12765 | 480 | -96.2% | 403 | 1 / 0 | 8860 / 0 |
| 100 | zero | 0 us | 1216 | 384 | -68.4% | 403 | 1 / 0 | 0 / 0 |
| 1000 | zero | 0 us | 1582 | 818 | -48.3% | 403 | 1 / 0 | 0 / 0 |
| 1000 | zero | 400 us | 12785 | 857 | -93.3% | 403 | 1 / 0 | 8860 / 0 |

(The full matrix is 7 counts x 2 words x 2 latency models x 6 legs; the table
above is the zero-word band plus the cap's nonzero row. Both latency models are
SYNTHETIC: nothing there measures reth's live latency.)

What the matrix establishes, and its edges:

- **The mechanism is the deleted batch plus the deleted serial read.** A ready
  fan-out dispatches one header read and no storage packet; a legacy or cold
  fan-out dispatches the header plus one prefetch batch whose member count is the
  fan-out's derived slot set — 11, 35, 131, 403 at 1, 4, 16, 50 candidates — plus
  one serial read. Packets are 6 legacy / 4 ready, and the dispatch leg's BAL
  fetch count is zero everywhere (the only BAL fetch in a ready leg is the
  background prepare's).
- **The zero-latency band (-41 % to -68 %) prices construction work**; the 400 us
  band (-93 % to -98 %) prices the wait. At the cap the legacy fan-out spends
  12498 us (zero word) / 12765 us (nonzero), of which 8860 us is the model's own
  storage wall, against 477 / 480 us.
- **The read half is measured, not asserted.** `on-ready-current` and
  `on-ready-previous` report zero dynamic reads at every count and word;
  `on-ready-unknown` omits exactly the candidate's read key and pays exactly one
  dynamic read whose value is the covered control's — the fallback fetched the
  right word, not merely some word. `off-legacy` and both cold controls pay that
  same single serial read, because no derivation can know a contract's own slot.
- **The read advantage is bounded by list coverage.** A key the settled list does
  not carry costs the legacy lane's own dynamic read, so a ready head buys
  nothing on those keys. Widening the served list is what widens the benefit, and
  a contract's arbitrary storage is exactly what today's derivation cannot reach.
- **The production cap is inside the measurement.**
  `dispatch_profitable_results` truncates to `MAX_SIMULATE_CONCURRENT = 50`
  (`rust/crates/engine/degenbot-arbitrage/src/dispatch.rs:187`). Counts
  100/500/1000 simulate the same 50 candidates and measure only the O(N)
  pre-filter over the caller's longer list, identically in both lanes; nothing
  above 50 is a wider fan-out.
- **The cold controls are the shipped lane.** Both cold legs resolve
  `legacy/lineage-refused` and `legacy/undelivered`, dispatch the same wire as
  `off-legacy`, report the same modeled wall, and produce identical verdicts: the
  fallback is the production path, not a degraded one.
- **The background cost is reported and not subtracted.** At the cap, settling
  three heads costs a covered-list prepare median of 48 us (zero model) / 1131 us
  (400 us model — the fetch's round trip) and an apply median of 189-222 us over
  404 applied writes. The fan-out is about 12 ms cheaper at the cap while the
  pipeline spent about 1.1 ms preparing and 0.2 ms applying per head; both
  numbers are the report's, and the subtraction is the operator's.
- **Verdict parity is exact and value-sensitive.** Within a word, every leg at
  every count produces the same rows, the same gross/net/gas and the same
  post-execution read set, and the probe exits 1 if any differs.

## Retention: an idle-lifetime bound

The opt-in's retention is one fixed policy: `Retention::IdleBlocks(8)` at the
default fold depth, chosen because eight is the largest threshold the retention
probe measured and because the live re-read gap distributions put p99 at or below
it in the windows used to decide it. Nothing larger was measured. The library
default for any caller constructing its own cache stays `Retention::Unbounded`.

The window bounds an entry's IDLE LIFETIME, never the store's total size. A
session whose active read/write working set keeps naming identities no candidate
has READ inside the last eight blocks keeps seeding new entries, and nothing in
this policy caps the count: size tracks the eight-block active set plus the fold
horizon. Eight blocks is what makes `held` describe the current working set
rather than a run's whole read history; it is not a retention ceiling, and
long-run RSS is unproven in exactly that sense.

Live identity counts, cross-capture and cross-head:

| quantity | v2 (before the idle window) | v3 (idle window) |
| --- | --- | --- |
| chain size at the last head | 6420 (`carried`; the capture predates `held`) | 2912 (`held`), `carried` 2751 |
| `live` at the last head | not reported | 2173 |
| `expired` at the last head | not reported | 739 |
| `held` / `live` / `expired` maxima | not reported | 3813 / 2302 / 1658 |
| heads / window | 27 | 38 |

Read it as a direction with three caveats that all matter:

- **Different captures.** v2 ran before the idle-window policy landed; v3 is a later
  window at different heads with a different write volume.
- **Different fields.** v2 could only report `carried` (the sealed parent's entry
  count); v3 reports both. Against v3's `held` the reduction is 54.6 %
  (6420 → 2912, the "about 55 %" figure); against v3's `carried` it is 57.1 %
  (6420 → 2751).
- **Identity counts are not memory.** One entry is a value plus a recency record,
  and no capture reports bytes or RSS.

The replay probes price the policy on real and synthetic read shapes:

| shape | effect of `IdleBlocks(8)` |
| --- | --- |
| `fanout_trace` replay (real recorded candidates) | live entries 3,704 → 1,154 (-69 %); +381 dynamic reads (+15.0 % of the delta leg's); storage wait +27.4 % / +16.8 % at the two price points, total wall +2.2 % / +5.2 % |
| `bal_broader_trace` replay | live entries 4,322 → 1,272 (-71 %); +144 dynamic reads (+5.5 %); total wall -0.6 % / -0.2 % |
| synthetic hot/warm/BAL-only stream, 300 blocks x 2,000 identities | live 1,860 → 690 (-63 %), held 1,860 → 452 (-76 %), run allocations 101,083 → 61,315 KiB (-39 %), zero carried-hit loss |
| read-heavy real coverage captures | a roughly 1:1 percentage trade (T=2: -19 % live for +17 % of reads); T=8 trims almost nothing |

The policy's cost is exactly the reads whose gap since their last read exceeds
the window, and its benefit is exactly the entries not read inside it. That is
why the threshold had to come from a gap distribution, and why the soak has to
re-check it.

### The fixed window against measured gaps

| capture | arm | re-read gap p99 | share inside 8 blocks | re-reads beyond 8 (max gap) | p99 <= 8? |
| --- | --- | --- | --- | --- | --- |
| v1 | OFF | 9 | 98.97% (21468 / 21692) | 224 (38) | no |
| v1 | ON | 17 | 96.61% (15950 / 16509) | 559 (28) | no |
| v2 | OFF | 8 | 99.25% (21842 / 22007) | 165 (25) | yes |
| v2 | ON | 6 | 99.75% (36755 / 36847) | 92 (17) | yes |
| v3 | ON | 16 | 98.23% (18544 / 18879) | 335 (32) | no |

The rounds replay's own distributions bracket the same band (p99 9 on the
narrower corpus, 8 on the broader).

**Eight blocks is reasonable for a soak and is not universally optimal.** v3's
p99 of 16 blocks means the window converts 335 of 18,879 re-reads (1.77 %) into
dynamic reads it would otherwise have carried; a wider window would convert
fewer and hold more. Nothing beyond eight was measured, so widening it is a new
measurement rather than a tuning decision — and the implementation deliberately
exposes no knob.

## Correctness evidence

Claims and the pins or probes that carry them (test names are the contract):

| claim | evidence |
| --- | --- |
| Exact-head fencing: a transition links only to `previous + 1` with a matching parent hash, and every other lineage settles cold | `CarryRefusal` set and `classify_link`; suite `bal_delta_snapshot.rs` |
| Gaps and reorgs stay correct: a skipped head settles cold and the next consecutive head carries; a same-height reorg supersedes and the new branch starts cold | `a_skipped_head_settles_cold_and_the_next_consecutive_head_carries`, `a_same_height_reorg_supersedes_and_the_new_branch_starts_cold` (`bal_head_pipeline.rs`); `a_same_height_reorg_selects_the_replacement_branch`, `a_same_height_reorg_retires_the_anchors_storage_memo` (`dispatch.rs`) |
| Zero or deletion finals never lose to a stale carried value; an unknown key seeds only under relevance; an irrelevant unknown write is counted, never hidden | write policy in `seed_final_writes`; suites `bal_retention.rs`, `bal_session_relevance.rs` |
| Relevance is the session's set (registered paths plus settlement identities), read at apply time, once per registry revision | `relevance_is_the_union_of_the_registered_paths_and_the_settlement_identities`, `a_path_registered_during_preparation_is_included_when_it_lands_before_the_apply`, `a_path_registered_after_the_apply_seeds_from_the_next_head_and_its_candidate_reads_dynamically` |
| Expiry is invisibility, never staleness: an expired read takes the dynamic path and answers the anchor's CURRENT word | 14 pins in `bal_retention.rs`; `both_real_fixtures_match_independent_cold_reads_at_every_anchor_with_retention` (zero mismatches) |
| Dynamic reads are round-coalesced and single-flight; a refused or abandoned round settles nothing | 11 pins in `bal_delta_rounds.rs`; a planted duplicate dispatches one member; a planted one-wei divergence is caught (30,132 mismatches, nonzero exit) |
| One previous generation is readable; a two-block lag is not | `a_lagging_fan_out_anchor_reads_the_previous_generation_and_a_two_block_lag_does_not`; the probe's `on-ready-previous` leg is `delta` at every count |
| Dispatch does no BAL work; a never-delivered or settle-cold head serves no parent value on the legacy lane | `dispatch_never_prepares_a_bal_and_a_never_delivered_head_takes_the_legacy_lane`, `an_explicit_failure_waits_once_then_the_legacy_prefetch_lane_works` |
| A waiter resolves on its own generation's end, and a superseded or gapped head is never served | `a_waiter_on_a_superseded_head_resolves_and_never_serves_the_abandoned_branch`, `a_waiter_across_a_gap_resolves_with_the_actual_current_reason` (`bal_head_lifecycle.rs`) |
| An abandoned or never-claimed preparation is terminal: its waiters resolve `abandoned`, a later explicit retry serves only NEW selections, and the fan-out takes the legacy lane | `an_abandoned_prepare_claim_wakes_its_waiters_with_the_terminal_abandonment`, `a_waiter_arriving_after_an_abandonment_resolves_immediately`, `a_delivered_head_with_no_preparation_owner_resolves_abandoned_without_parking`, `a_later_explicit_retry_settles_the_head_for_new_callers_only` (`bal_head_lifecycle.rs`); `an_abandoned_preparation_resolves_the_selection_on_the_legacy_lane_with_the_abandoned_reason`, `a_delivered_head_nothing_prepared_resolves_the_selection_with_the_abandoned_reason`, `an_abandoned_preparation_takes_the_fan_out_to_the_legacy_lane_with_the_abandoned_reason` (`dispatch.rs`) |
| OFF is the shipped path byte for byte, and requests nothing | `the_off_lane_is_byte_identical_to_the_shipped_legacy_path`, `the_opt_in_off_makes_no_bal_request_and_keeps_the_prefetch_lane` |
| The storage representation changes the reads and never the verdict | `the_storage_selection_changes_the_reads_and_never_the_verdict`, `candidate_verdicts_and_failure_buckets_are_identical_with_the_pipeline_on_and_off` |
| An unsupported BAL endpoint, no parent, an explicit terminal failure and a superseded head each take the legacy lane with their own label | `an_unsupported_bal_endpoint_takes_the_legacy_path_with_its_reason`, `a_lineage_refused_head_takes_the_legacy_path_with_its_reason`, `an_explicit_failure_waits_once_then_the_legacy_prefetch_lane_works`, `the_cold_reason_vocabulary_is_the_closed_set` |
| A same-block in-flight head waits for its settle and resolves delta; the previous anchor does not wait; duplicate fan-outs join one completion | `a_same_block_in_flight_head_waits_for_the_settle_and_resolves_delta`, `the_previous_block_does_not_wait_while_the_current_head_is_in_flight`, `duplicate_fan_outs_join_one_completion_without_duplicate_bal` |
| Value-sensitive parity at dispatch scale, both words, every count | 8 pins in `rust/crates/engine/degenbot-arbitrage/tests/bal_dispatch_scaling.rs`; the probe's own fixture gates exit 1 on any divergence |

State of the suites at the evaluated commit: `degenbot-simulation` 478 passed,
25 skipped; `degenbot-arbitrage` 193 passed, 0 skipped. The whole-workspace gate
(`just test`) and the live/probe ladder named under "Regenerating the evidence"
are the epic's own ladder; this document does not re-derive them.

## Limits

1. **The controlled corpus is one read identity and one route shape.** It reads a
   single executor slot with a sub-100-byte runtime over repeated market paths
   with distinct pool identities: the scaling claim is about fan-out WIDTH, never
   about a candidate mix. A productive read set is many slots across pools,
   tokens and hooks.
2. **The receipt is synthetic.** `gross_profit` is the frame's answer to the
   scripted word, not a swap's retained profit; no gross/net figure in the probe
   is an economics claim.
3. **The covered claim is conditional.** It measures "a list that carried the
   key", with one fixture address appended to the relevance set by hand. A
   production relevance set that never names a contract's own storage would
   filter that write out.
4. **The legacy lane's dynamic read count is one per fan-out here.** The first
   fallback answer is memoized at the anchor, so distinct unknown keys per
   candidate — the realistic wide-read case — remain unmeasured.
5. **The production candidate cap is 50.** Nothing claims more than 50 simulated
   candidates; counts above the cap measure the pre-filter only.
6. **The live windows are non-paired and lossy** (see "What the live windows
   still cannot price").
7. **No long soak exists.** The three windows are minutes long; nothing observes
   a multi-hour run, and nothing observes a reorg storm, a BAL-endpoint outage or
   a supervisor restart under the flag.
8. **Memory is identity counts, never bytes or RSS, and no capture caps the
   store.** The idle window bounds an entry's lifetime, not the number of entries:
   a continually novel active read/write working set keeps seeding, so size
   tracks that set plus the fold horizon and long-run RSS stays unproven. The
   nearest proxy in the evidence is the retention replay's run allocations in
   KiB, and it is a replay.
9. **The BAL source requires ONE of the two official EIP-7928 endpoints.** An
   endpoint serving neither `debug_getRawBlockAccessList` nor
   `eth_getBlockAccessList` makes every head `bal-unreadable`, and the evaluation
   itself ran without an up-front check. That startup fail-fast check now exists
   (`probe_bal_capability` in
   `rust/crates/engine/degenbot-simulation/src/sim/evm2/bal_capability.rs`): an
   ON session's batch-executor construction resolves the current canonical head's
   exact `(number, hash)` and acquires + fully decodes its list through the
   shared `fetch_bal_seed` seam BEFORE arming the pipeline, so an unusable
   endpoint is a construction error instead of a per-head `bal-unreadable`
   collapse to the legacy lane. OFF stays request-free, and no new transport code
   was needed: the probe is the provider's `get_block_access_list` acquisition
   under the same unsupported/closed classification. The same-block readiness
   wait this evaluation did not cover has since landed (post-evaluation
   change): a fan-out at the exact in-flight head joins the pipeline's
   preparation through the driver's per-head completion notification and
   re-reads the exact anchor after the settle — see the readiness facts above.
10. **`MarketContext` installs the flag but consumes no generation.** It passes
    the warm cache to its per-block frame handle
    (`rust/crates/engine/degenbot-strategy/src/frame_pipeline.rs:561`), never
    reaches `dispatch_across_deployments`, and installs neither the canonical-head
    sink nor the armed pipeline, so turning the flag on there yields no BAL
    request and no delta read.
11. **The pure-Rust settlement example runs structurally OFF**: it passes
    `warm_cache: None` (`rust/examples/settlement_bot/src/main.rs:1226`), so every
    fan-out mints the default (OFF) cache.
12. **Head distributions are thin.** Each head appears once per capture and the
    burst regime rests on a handful of heads, so median/p90/max across heads
    describe those windows and never a distribution over heads.

## Soak acceptance criteria

Run the opt-in (not default ON) for a stated duration on the production
composition, with an OFF arm available for matched-head comparison. Promote to
default ON only after every criterion passes. Recovering from a failure is
configuration alone (see the rollback), but PASSING a criterion is not free of
engineering: each one names the measurement it needs, and where today's rows do
not carry that measurement the soak carries the instrumentation work (a p95 wall,
a read-origin counter, an RSS sampler) instead of assuming it away.

1. **Sustained readiness after warmup.** Excluding the boot tail, the `delta`
   share of fenced fan-outs stays at or above the v3 rate (10 of 11), and no
   settled head fails to serve its own anchor.
2. **Cold-reason census.** Report the closed-label distribution per 1,000 heads
   and per 1,000 fenced fan-outs. `undelivered` should
   appear only at the boot edge (each later window had exactly one `undelivered`,
   at boot); a sustained `lineage-refused` rate means the pump is skipping headers
   or reorging; `bal-unreadable` means the endpoint serves neither BAL
   representation (or answered one unreadably); `abandoned` means a head's
   preparation ended without a BAL verdict (or none ever claimed it), so its
   fan-outs paid the legacy lane.
3. **Verdict and failure parity.** For matched heads — or, better, one replayed
   candidate set through both lanes — verdicts, failure buckets and per-candidate
   read sets are identical, as the controlled probe already pins at one head. Any
   divergence is a stop.
4. **Held/live slope plus actual RSS.** Record `held`/`live`/`expired` per head
   and the process RSS at a fixed cadence for the whole run. `held` must plateau
   at the fold horizon rather than grow monotonically (v2's unbounded chain
   reported 6420 carried identities at its 27th head; v3's chain under the idle
   window ended at a `held` of 2912), and RSS must plateau inside a band the
   operator declares before the run. Neither number exists yet. The band is the
   criterion precisely because the window bounds lifetime and not size: a novel
   working set that outruns the fold horizon has nothing to stop it but the
   operator's band.
5. **Background prepare p95 against the head interval.** Record prepare p95 (and
   max) beside the observed head interval. The soak passes only if p95 stays far
   below the interval, so readiness keeps beating the one-block solve lag. The
   current evidence reports median/p90/max, not p95: extend the capture or the
   report rather than reinterpreting these windows, and allow for a p95 field (or
   a small analysis change) if the `bal-head` row cannot supply it.
6. **Fallback RPC cost.** Count the legacy lane's cost while the flag is on: one
   prefetch batch per fallback (11..403 members at the probe's widths), one serial
   read per fan-out, plus one header read per fan-out and one BAL fetch per
   accepted head. A fallback rate high enough to erase the delta lane's saving is
   a NO-GO even with perfect correctness. The serial read is the one term the
   current rows do not price per fan-out: the `cands=` line's cold-storage
   counters aggregate every cold read the fan-out paid, the prefetch row counts
   the batch and not the read, and no live row records read origin. Counting it
   may need a read-origin counter, and that work belongs to the soak.
7. **No promotion to default ON** until 1-6 pass over the agreed duration with
   the numbers attached to the review. Default ON is a separate decision from
   "the flag works".

**Rollback** is one environment change and a restart: set
`DEGENBOT_DELTA_DISPATCH=0` (equivalently `simulation.delta_dispatch = false`)
and restart the session. There is no migration, no persisted generation, and no
file or database state to reverse — the driver, its generations and the idle
window live only in the session's memory, the flag is read once at construction,
and an OFF session installs no pipeline and issues no BAL request.

## Commit map

In order, from the opt-in's first commit to the retention commit (verified in
`git log b1788d091^..cfaf61199`):

| commit | subject | what it landed |
| --- | --- | --- |
| `b1788d091` | feat(simulation): add opt-in BAL delta dispatch | the flag, the session driver, storage selection, and the OFF-identical legacy lane |
| `b859c8839` | test(dispatch): add live BAL comparison harness | `scripts/bal_live_ab_analysis.py`, the `just bal-live-ab-report` recipe, and its tests |
| `be839880c` | feat(simulation): prepare BAL heads on canonical arrival | the canonical-head pipeline and its sink |
| `cd7150b7d` | feat(simulation): seed BAL writes from session relevance | apply-time session relevance, per registry revision |
| `854f94082` | feat(simulation): make BAL dispatch query-only | dispatch asks, never delivers or prepares; the closed fallback labels; the per-head `bal-head` row and the additive per-fan-out `fanout` id |
| `ce7eec496` | test(dispatch): report attributed BAL comparisons | the attributed report over that existing `fanout` id — report and analysis corrections only, no new key and no new row |
| `efda69708` | test(dispatch): benchmark BAL prefetch scaling | the scaling probe and its report tables |
| `72634a313` | test(dispatch): measure BAL candidate storage reads | the value-sensitive SLOAD corpus and the covered/unknown controls |
| `cfaf61199` | feat(simulation): bound BAL dispatch snapshot retention | the fixed eight-block idle window and the held/live/expired reporting |

## Regenerating the evidence

Live report: two windows that differ only in the flag. The launcher writes
`logs/bot_run.log`, which is copied after each run.

```bash
# OFF window
DEGENBOT_DELTA_DISPATCH=0 DEGENBOT_DEBUG=1 \
  RUST_LOG='degenbot=debug,degenbot::sim=trace' \
  ./run_bot.sh start     # ... observe ... ./run_bot.sh stop
cp logs/bot_run.log logs/bal_dispatch_v2_off.log

# ON window
DEGENBOT_DELTA_DISPATCH=1 DEGENBOT_DEBUG=1 \
  RUST_LOG='degenbot=debug,degenbot::sim=trace' \
  ./run_bot.sh start     # ... observe ... ./run_bot.sh stop
cp logs/bot_run.log logs/bal_dispatch_v3_on.log

# analysis (parameterized; each report names its own capture files)
just bal-live-ab-report \
  off=logs/bal_dispatch_v2_off.log on=logs/bal_dispatch_v3_on.log \
  out=logs/bal_live_ab_v3_result.md \
  off_trace=logs/bal_dispatch_v2_off_trace.log \
  on_trace=logs/bal_dispatch_v3_on_trace.log
```

`just bal-live-ab-report` exits 1 when a capture check fails (an OFF window that
selected `delta`, a differing executor image, RPC or snapshot seed, a trace
extract that disagrees with its capture) and still writes the report, so a
failing pair is read, never silently accepted.

Controlled mechanism, retention and corpus probes:

```bash
# the whole scaling matrix (counts 1,4,16,50,100,500,1000 x words zero/nonzero
# x zero/400us latency x 3 interleaved reps)
just bal-dispatch-scaling-probe

# a narrower or wider sweep, more repetitions
BAL_DISPATCH_SCALING_COUNTS=1,50,1000 BAL_DISPATCH_SCALING_REPS=9 \
  just bal-dispatch-scaling-probe

# retention policy, and its negative probe (the negative MUST exit 1)
just bal-retention-probe
BAL_RETENTION_PLANT_DIVERGENCE=1 just bal-retention-probe

# round-coalesced reads and the measured idle window
just bal-delta-rounds-probe

# production relevance against the recorded-read oracle
just bal-production-relevance-probe
```

The probe pins that keep those harnesses honest ride the default suite:

```bash
just test-rust-nextest -p degenbot-simulation
just test-rust-nextest -p degenbot-arbitrage
just test-rust-nextest -p degenbot-arbitrage -E 'binary(bal_dispatch_scaling)'
```

## Factual uncertainty in this audit

1. **The evidence is untracked.** `logs/` is gitignored (`.gitignore:239`), so
   every figure here is verifiable only in a tree that carries those files; a
   fresh clone cannot re-check them.
2. **v3 has no OFF run of its own.** Both `bal_live_ab_v2_result.md` and
   `bal_live_ab_v3_result.md` pair their ON window against the SAME OFF capture
   (`logs/bal_dispatch_v2_off.log`), so every OFF-side figure attributed to v3 is
   v2's capture.
3. **The reports' OFF-arm explanation is a template.** Both later reports say the
   OFF capture "predates" the `bal-head` row
   (`scripts/bal_live_ab_analysis.py:1459` is unconditional). The structural
   reason is stronger: an OFF session installs no pipeline and emits no head
   transition, so an OFF arm can never carry those rows. Its per-head walls are
   unmeasurable by construction, not by capture vintage.
4. **One prose figure in the scaling report disagrees with its own table.** The
   "Reading the numbers" paragraph gives the unknown control at the cap as
   2548 us (zero word); the probe table's count-50 / 400 us median for
   `on-ready-unknown` is 1490 us (2574 us is that leg's max). This document uses
   the table. The nonzero-word figure there (1510 us) matches its table.
5. **The held reduction is approximate by construction** (see "Retention: an
   idle-lifetime bound"): cross-capture, different heads, and v2 could report only
   `carried`. -54.6 % against v3's `held`; -57.1 % against v3's `carried`.
6. **v1 does not support the fixed window.** Its OFF arm's p99 was 9 blocks and
   its ON arm's 17, so neither met the p99 <= 8 precondition the policy was
   chosen under. v2 (both arms) and one replay corpus did; v3's ON arm did not.
7. **The prepare-wall improvement between v2 and v3 is confounded** (different
   windows, different write volume, retention on in v3), and no capture separates
   those causes.
8. **The exact commands that produced the v2 and v3 reports are not recorded** in
   either report beyond `just bal-live-ab-report`; the regeneration block gives
   the recipe's parameterized form with the capture files those reports name, not
   a verbatim transcript.
9. **This audit ran no live capture and no probe.** It re-read the reports and the
   committed code at `cfaf61199`, ran the two scoped crate suites quoted above,
   and ran the repository's Markdown and comment-hygiene checks. The `just test`
   ladder and the live-RPC lanes belong to the manager's gate, not to this
   document.
