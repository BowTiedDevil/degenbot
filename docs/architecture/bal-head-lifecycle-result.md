# The asynchronous canonical-head lifecycle for BAL delta snapshots

A head's block access list is fetched and fully decoded as its own **prepare**
phase, off the caller's critical path, and applied to the snapshot when — and only
when — it names the exact head the driver currently holds. This document records
the seam as it stands, the correctness evidence behind it, one live measurement on
reth, and the executable comparison that has replaced a hand-read numbers table.

Scope: the lifecycle and its measurements. No candidate dispatch integration, no
production relevance derivation, no retention redesign, no wire/ABI/FFI/CLI
contract.

## The seam

`BalHeadDriver` owns one `DeltaSnapshotCache` and drives its transition across a
head stream. The synchronous `advance_bal` is unchanged and stays the control.

| surface | what it does |
| --- | --- |
| `deliver(anchor, parent_hash) -> HeadTicket` | records the head by its **exact anchor** and mints the one cold generation callers read until the apply lands. A repeat of the current head (same anchor, same parent hash) returns the same ticket and does no work; a head whose anchor the cache already has installed settles on the spot with the readiness its own settle derived, so a cold head stays cold with its reason |
| `prepare(ticket) -> PreparedBal` | the prepare phase: fetch the list at the ticket's exact hash and decode it wholesale. Moves no snapshot state, so a preparation that finishes late is simply dropped |
| `apply(ticket, &prepared, relevance) -> HeadOutcome` | the apply: one synchronous transition, the control's transition with the fetch already done — same lineage classification, same sealed parent chain, same write policy, same retention/compaction behaviour. `relevance` names the addresses whose new writes may SEED; a write to a key the generation already serves always overwrites. The report's `bal_latency` is the prepare phase's own wall time |
| `apply_members(ticket, &prepared, &RelevanceSet)` | the same transition with the session's relevance already materialized as a membership set, so a per-head apply pays no conversion. `apply` is this method with the caller's slice converted first |
| `advance(ticket, relevance) -> HeadOutcome` | prepare + apply in one await under one single-flight claim: a second concurrent caller gets `InFlight` instead of a duplicate fetch, and a dropped (cancelled) call releases the claim so a later caller prepares the head again |
| `advance_with_relevance(ticket, resolve)` | `advance` with the session's relevance resolved AFTER the list is prepared and immediately BEFORE it applies, at most once per head |
| `snapshot(anchor) -> HeadView` | the read seam, never blocking, in this order: the installed generation whenever that exact anchor settled (it keeps answering after the next head arrives); the current head's cold generation while its BAL is still preparing; the PREVIOUS anchor's generation when the installing transition linked; a fresh cold generation carrying `Superseded` or `Undelivered` |
| `wait_for_head(anchor) -> HeadView` | the deterministic wait a same-block caller joins the head's existing preparation with: registers on the exact `(anchor, seq)` generation's current claim attempt ONLY while that attempt is genuinely live, resolves on that attempt's terminal completion, then re-reads the anchor. Never `Unprepared`, never parks without a live owner, never ends on a wall clock, fetches nothing, claims nothing, applies nothing, and holds no lock across the wait |
| `completion(anchor) -> Option<HeadCompletion>` | the current attempt's published terminal result (`Settled` / `Superseded` / `Abandoned`), readable without waiting; `None` while undecided |
| `stats() -> HeadStats` | delivered / repeated / superseded / settled / already_current / discarded / ready_views / cold_views |
| `installed_hub(anchor)` (cache), `delivered()` / `installed()` / `previous()` (driver) | read-only lookups: `installed_hub` clones the cache's installed handle and never mints, replaces, or evicts a generation, so a reader cannot install one by looking |

`HeadCold` names every cold reason a caller can see: `Unprepared` (delivered, list
not applied yet), `Superseded` (a later canonical head replaced this one before it
settled), `Undelivered` (no head with this exact anchor was ever delivered),
`Abandoned` (the head's claim ended without applying — a dropped advance — or no
preparation ever claimed it), and `Settled { lineage, bal }` for a head that
settled without carrying (a lineage refusal, an unreadable list, or both).

### The per-claim lifecycle

Completion is scoped to a claim ATTEMPT, not to a head. A head is delivered once
but can be claimed more than once: a dropped advance releases its claim and a later
caller prepares the same anchor again. Each attempt mints its own epoch and its own
one-shot completion channel, so a retry publishes on ITS channel and can never
rewrite an abandonment an earlier attempt's waiters have already been answered
with. The ticket's `seq` is the only sequencing a caller sees; a retry reuses it.

That is what makes the three wait answers terminal and distinct: a settle answers
the settled generation (ready, or cold with the exact `Settled` reason), a
supersession answers `Superseded`, and a generation no live preparation owns
answers `Abandoned` at that exact anchor. Several waiters share one attempt's one
completion and one fetch; dropping one waiter strands nobody and cancels nothing;
a settle that lands between a wait's first look and its registration resolves
immediately. An out-of-band `apply` on a head whose only claim already ended
publishes its settle on a fresh private epoch, so it answers the head's CURRENT
attempt without retroactively reversing the earlier abandonment.

### What a caller is guaranteed

- A view at a delivered head serves **no parent value** until that exact head's
  list applied. An unsupported, missing, malformed, or foreign list is the
  existing cold path with the reason reported — never a stale parent value at the
  wrong anchor.
- A prepared list applies only to the head it was prepared for. Three fences: the
  list must carry the ticket's exact anchor (`Foreign`), the ticket must still be
  the driver's head (`Superseded`), and the transition re-classifies lineage
  against whatever the cache actually installed.
- Reads never wait on a preparation: `snapshot` is synchronous, so an unprepared
  head costs a dynamic read, not a blocked candidate loop.

## Correctness evidence

`tests/bal_head_lifecycle.rs` — 55 pins, every name stating the behavior:

- **transition parity** — `applying_a_prepared_list_matches_the_synchronous_advance_item_for_item`,
  `a_prepared_list_applies_only_to_the_head_it_was_prepared_for`,
  `a_late_preparation_for_a_superseded_head_installs_nothing`,
  `a_same_height_reorg_settles_cold_and_the_next_head_carries_only_the_new_branch`,
  `a_gap_settles_cold_and_the_next_consecutive_head_re_establishes_the_chain`,
  `a_number_only_head_prepares_no_list_and_settles_cold`,
  `an_unreadable_list_settles_a_cold_head_with_the_skip_reported`,
  `the_apply_captures_the_parent_handle_it_seals`.
- **readiness and the cold path** — `a_delivered_head_serves_the_cold_path_until_its_bal_applies`,
  `a_settle_into_an_unprepared_head_stays_handle_local`,
  `concurrent_reads_at_an_unprepared_head_share_one_cold_generation`,
  `a_settled_head_keeps_answering_after_the_next_head_arrives`,
  `a_delivery_of_the_installed_head_settles_without_bal_work`,
  `a_redelivered_installed_cold_head_keeps_its_settle_derived_readiness`,
  `the_previous_anchor_answers_from_the_generation_that_was_installed_there`,
  `the_previous_window_reports_the_cold_readiness_its_generation_settled_with`,
  `an_anchor_two_below_the_installed_head_is_cold_and_never_served_from_the_newer_generation`.
- **claims and waits** — `an_idempotent_delivery_and_advance_repeat_no_bal_work`,
  `a_cancelled_preparation_releases_the_claim_and_the_head_retries`,
  `a_concurrent_advance_joins_the_claimed_preparation_without_a_second_fetch`,
  `a_waiter_joins_the_in_flight_preparation_and_resolves_with_the_settled_values`,
  `several_waiters_share_one_terminal_completion_and_one_fetch`,
  `dropping_one_waiter_never_strands_the_others_or_cancels_the_preparation`,
  `a_settle_between_the_first_look_and_the_registration_resolves_the_wait_immediately`,
  `a_waiter_at_a_head_whose_list_cannot_be_read_resolves_with_the_real_cold_reason`,
  `a_waiter_at_a_lineage_refused_head_resolves_with_the_lineage_reason`,
  `a_waiter_on_a_superseded_head_resolves_and_never_serves_the_abandoned_branch`,
  `a_waiter_across_a_gap_resolves_with_the_actual_current_reason`,
  `an_abandoned_prepare_claim_wakes_its_waiters_with_the_terminal_abandonment`,
  `a_waiter_arriving_after_an_abandonment_resolves_immediately`,
  `a_delivered_head_with_no_preparation_owner_resolves_abandoned_without_parking`,
  `a_later_explicit_retry_settles_the_head_for_new_callers_only`,
  `an_abandoned_claims_waiter_resolves_abandoned_even_when_the_retry_settles_first`,
  `a_repeat_delivery_of_a_live_preparation_reuses_its_claim_attempt_and_waiters`,
  `a_cancelled_retry_is_terminal_for_its_own_waiters`,
  `an_apply_settles_the_current_attempt_and_never_rewrites_an_earlier_abandonment`,
  `a_supersession_of_a_retried_attempt_leaves_the_earlier_abandonment_terminal`,
  `a_redelivery_with_a_different_parent_never_rewrites_an_abandoned_attempt`,
  `an_out_of_band_apply_settles_an_abandoned_heads_current_completion`,
  `a_direct_prepare_settles_an_unclaimed_head_without_a_claim_attempt`,
  `a_distant_supersession_keeps_only_the_last_abandoned_anchor`.
- **retention and compaction** — `the_lifecycle_compacts_on_the_control_threshold`,
  `the_lifecycle_applies_the_callers_retention_policy`.
- **replay evidence** — `every_generation_of_a_modeled_chain_settles_identically_through_the_lifecycle`,
  `the_real_coverage_capture_settles_every_head_with_zero_divergence`,
  `the_full_clone_control_agrees_with_the_lifecycle_transition`.
- **counters and stored reports** — `the_lifecycle_counters_count_exactly_the_transitions_it_ran`,
  `an_advance_stores_the_report_it_settled_with`,
  `a_retry_settles_an_abandoned_head_with_its_own_report`,
  `an_out_of_band_apply_stores_the_report_it_settled_with`,
  `a_second_settle_at_the_same_head_never_keeps_the_older_report`,
  `an_installed_redelivery_stores_no_report_of_its_own`.
- **the unbounded read fast path and installed-handle reads** —
  `an_unbounded_chain_reports_held_equal_to_live_at_every_head`,
  `repeated_reads_at_the_installed_head_share_one_handle_and_move_nothing`,
  `a_cold_read_at_an_undelivered_anchor_installs_and_evicts_nothing`.

The unchanged suites stay green and are the evidence that the transition did not
move: `tests/bal_delta_snapshot.rs` (11 pins, full-clone parity),
`tests/bal_retention.rs` (14 pins, retention semantics), and
`tests/bal_delta_rounds.rs` (11 pins, the round-coalesced read seam). The control
itself (`ReadHubCache::advance_bal` and `DeltaSnapshotCache::advance_bal`) runs
through the same `PreparedBal` fetch/decode stage the lifecycle uses, so the two
cannot drift in the stage that owns the RPC.

The lifecycle replay compares, per block, the whole report (outcome, applied
writes, overwrite and skip counts), the snapshot entry count, and every resolved
value against the synchronous delta control — on a modeled chain and on the
committed real write-coverage capture (`tests/fixtures/bal_real_coverage.json`,
21 blocks, 2,543 tracked identities). Zero divergences.

## The live measurement

The probe is `just bal-head-lifecycle-probe` against the devcontainer's reth
(`reth/v2.11.0-b9abbcc`, chain 1, archive). One session, 2026-11-12, at the pinned
invocation

```sh
BAL_HEAD_SUMMARY_OUT=<local path>/smoke-1.json \
BAL_HEAD_FRESH=0 BAL_HEAD_FIRST=26375033 BAL_HEAD_LAST=26375053 \
just bal-head-lifecycle-probe
```

read 21 historical heads the node had no list cached for, applied each one, and
verified 2,995 identities through the settled generation against an independent
cold `eth_getStorageAt` batch at the same anchor: **zero mismatches**, 21/21 cold
first requests, every delivered head proved cold (empty generation) before its
apply.

### Stage walls (microseconds)

| leg | heads | deliver p50 | prepare p50 | prepare p95 | prepare max | repeat p50 | apply p50 | apply p95 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| historical, node-cold | 21 | 1.42 | 704,180 | 1,470,000 | 2,360,000 | 1,060 | 604 | 1,212 |

| waiter leg | samples | wait p50 | prepare p50 | resolve p50 | resolve p95 |
| --- | ---: | ---: | ---: | ---: | ---: |
| claimed advance + `wait_for_head` | 21 | 21,300 | 21,150 | 152 | 380 |

`repeat` is a second fetch of the same hash in the same process and is the node's
own list-cache floor: ~1 ms, i.e. the RPC and the decode of a 40–500 KB list. The
waiter leg's `prepare` (21.15 ms) is dominated by the 20 ms parked window each
sample waits out on the closed transport gate, which lands inside the wait and
inside the prepare alike; `resolve` is the wait NET of that prepare — the settle,
the wake, and the wait's own re-read, i.e. what a same-block caller pays after the
preparation it joined finishes.

Read the table as three regimes:

1. **A list the node has to compute costs 0.14–2.36 s** (p50 0.70 s, p95 1.47 s,
   max 2.36 s over this stretch). That is 1–20 % of a 12 s block interval and far
   beyond any candidate decision window.
2. **A list in reth's cache costs ~1 ms**, whether the cache was warmed by an
   earlier request in this process or by reth's own prewarming of a new head. An
   earlier session's prewarmed fresh-head leg (`BAL_HEAD_FRESH=20`, then `6`)
   answered every first request in ≤2.1 ms (median 0.71 ms), so the prewarm window
   covered at least ~4 minutes of chain; that leg is not part of the pinned
   invocation above and is reproduced by setting `BAL_HEAD_FRESH`.
3. **The apply is a sub-millisecond synchronous transition** (p50 0.60 ms, p95
   1.21 ms at ~1.5 k snapshot entries), and the delivery is ~1 µs. The apply grows
   with the snapshot and the compaction fold, not with the block.

`prepare` is the whole fetch+decode stage and `apply` excludes it, so the split is
exact: the only part a candidate can ever wait on is the part the caller chose to
run off its critical path.

### Readiness and the wait

There is no readiness deadline and no timed fallback in the driver. `snapshot` is
synchronous and answers cold while a head prepares, with the reason reported; a
caller that wants the settled answer calls `wait_for_head`, which joins the head's
live claim attempt and resolves terminal (settled, superseded, or abandoned). The
probe therefore reports each head's prepare wall against candidate windows as a
count of preparations that would have outlasted them — a hypothetical, not driver
state:

| node state | 25 ms | 100 ms | 500 ms | 1 s | 2 s |
| --- | ---: | ---: | ---: | ---: | ---: |
| historical, node-cold (21 heads) | 21/21 | 21/21 | 15/21 | 4/21 | 1/21 |

So the two node states are cleanly separated: a node-cold stretch is a 100 %
beyond-100 ms regime, while a prewarmed fresh head's first request is ready in
~1–2 ms — far inside any candidate window. What a same-block caller pays once it
joins the preparation is the waiter leg's `resolve` (p50 152 µs, p95 380 µs here),
not the fetch.

## The executable comparison

The lifecycle's regression rule used to be read by eye off two p50/p95 lines. It is
now a command with a fail-closed contract.

**The summary.** A probe run writes one JSON object when `BAL_HEAD_SUMMARY_OUT`
names a path (the human report is unchanged and remains the measurement):

```json
{
  "schema": "bal-head-lifecycle/1",
  "invocation": {"first": 26375033, "last": 26375053, "fresh": 0, "wait_samples": 21},
  "historical": {"heads": 21, "apply_p50_ns": 604085, "apply_p95_ns": 1212241},
  "waiter": {"samples": 21, "resolve_p50_ns": 152054, "resolve_p95_ns": 379949}
}
```

Every compared field is an integer nanosecond count, so no rounding happens twice.
Adding a field is additive; changing what a field means bumps the version.

**The rule.** Exactly three `before` summaries and three `after` summaries. Per
metric the comparison takes the **median** of the three runs on each side — the
smallest sample that survives one outlier without inventing a distribution — and
passes while every after median is `<= before median * 1.10`. Growth strictly
beyond 10 % fails; exactly 10 % passes, decided in integer arithmetic (`after *
100 <= before * 110`). The four compared metrics are the historical leg's apply
p50/p95 and the waiter leg's resolve p50/p95; no whole-run wall clock is compared,
and no absolute number is a budget.

```sh
# three before runs, then the change, then three after runs
BAL_HEAD_SUMMARY_OUT=$PWD/.scratch/bal-head-lifecycle/before-1.json \
  BAL_HEAD_FRESH=0 BAL_HEAD_FIRST=26375033 BAL_HEAD_LAST=26375053 \
  just bal-head-lifecycle-probe
# ... before-2, before-3, then the same three as after-1..after-3

just bal-head-lifecycle-compare \
  --before .scratch/bal-head-lifecycle/before-{1,2,3}.json \
  --after  .scratch/bal-head-lifecycle/after-{1,2,3}.json
```

It prints each side's three samples and a review table of medians, growth, and
verdict, then answers one exit code:

| exit | meaning |
| ---: | --- |
| 0 | every metric's median within 10 % |
| 1 | at least one metric's median grew beyond 10 % (the offending rows are named) |
| 2 | the comparison was REFUSED: missing file, unparsable JSON, a foreign schema identity, a missing metric field, a non-integer or negative count, a p95 below its own p50, a leg that measured nothing, a zero before median, or a trio of the wrong size |

Exit 2 is the fail-closed half of the contract: an unreadable sample is not a fast
one, and a ratio against a zero before median is undefined rather than a pass. The
comparator's own pins are `tests/dispatch/test_bal_head_lifecycle_compare.py`
(`just py pytest tests/dispatch/test_bal_head_lifecycle_compare.py`), which drive
synthetic trios: the 10 % boundary, growth beyond it, medians versus means, and
each refusal above.

Absolute timings stay local. The summary and the trios a session runs live under
`.scratch/bal-head-lifecycle/` (git-ignored); this document records ONE session's
observation, not a committed baseline, and the rule compares whatever summaries the
caller points it at.

## What this measurement does not establish

- No candidate is executed: this is storage-read fidelity and stage latency, not an
  EVM verdict or an economic result.
- The relevance input is the capture's write addresses (ORACLE), not
  production-derived relevance.
- One node, one archive depth, one 21-block historical stretch. The fresh-head leg
  above is an earlier session's sample. No long bot capture, no lock-contention or
  RSS modelling, no soak.
- The prewarm window was measured over the last 20 heads (~4 min); how reth behaves
  with prewarming disabled or under memory pressure is not measured here.

## GO / NO-GO for a feature-flagged dispatch vertical slice

**GO**, on these conditions.

The correctness conditions the epic's definition of done requires are met for this
seam: zero value divergence against an independent cold read, zero lineage
divergence against the synchronous control, parent values withheld until the exact
head's list applies, a cold fallback with a reported reason on every failure and
late-response path, and no blocking read. The latency condition is met where the
node prewarms lists: a head is ready in ~1–2 ms after delivery, the apply is a
sub-millisecond synchronous step, and the BAL work is entirely removable from the
candidate's path by construction.

The conditions the slice must carry:

- **Gate it on the node's list readiness.** The 0.14–2.36 s cold-compute regime is
  real. A feature flag must be paired with a readiness policy sized to the candidate
  window — a caller that cannot afford the wait reads the cold generation instead —
  and the driver's cold reasons plus `stats()` must stay exported so a deployment
  that loses prewarming is visible instead of silently cold.
- **A vertical slice must still prove an end-to-end win.** Nothing here measures a
  candidate's wall time with the snapshot: the eliminable share of candidate wall
  time was the epic's step-2 question, and this task's numbers say the *BAL stage*
  can leave the critical path, not that candidate latency improves.
- **Dispatch, production relevance derivation, and any retention redesign stay out
  of this seam.** The driver's relevance input remains the caller's (a slice, or a
  session-resolved `RelevanceSet` applied at apply time), the read seam ends at
  `HeadView`, and the retention policy is unchanged (measured retention behavior is
  `logs/bal_retention_result.md`).
- **The regression rule is the comparison command, not a remembered number.** A
  lifecycle change is certified by three before/three after sessions of the pinned
  invocation through `just bal-head-lifecycle-compare`, not by re-reading this
  table.

## Development notes

- Source of truth for the transition: `advance_bal` and `apply_prepared` share one
  implementation; the lifecycle's `apply` is synchronous by design so the lineage
  check, the seal, and the anchor move are one transition under one lock hold.
- The provisional generation is deliberately cold and is dropped at settle: its
  settles do not cross into the settled generation (pinned by
  `a_settle_into_an_unprepared_head_stays_handle_local`). Reusing them would be a
  semantic addition, not part of this seam.
- The installed generation outlives its own head: after the next head is delivered,
  a query for the older exact anchor still returns that generation and its
  readiness, so a candidate mid-flight does not fall to a fresh cold hub when the
  head stream moves on (pinned by
  `a_settled_head_keeps_answering_after_the_next_head_arrives`). The read is a
  handle clone from the cache's installed anchor (`installed_hub`), so a read can
  neither install nor evict a generation.
- An unbounded-retention generation resolves a value and counts its live identities
  without walking the chain for recency records, because `Retention::Unbounded`
  expires nothing — the answer is the value lookup alone, and the live count is the
  chain's own identity arithmetic. The policy path stays the reference the fast path
  is pinned against
  (`an_unbounded_chain_reports_held_equal_to_live_at_every_head`).
- Reproduce: `just bal-head-lifecycle-probe` (knobs `BAL_HEAD_FIRST`,
  `BAL_HEAD_LAST`, `BAL_HEAD_FRESH`, `BAL_HEAD_WAIT_SAMPLES`,
  `BAL_HEAD_WAIT_HOLD_MS`, `BAL_HEAD_SUMMARY_OUT`, and
  `BAL_HEAD_PLANT_DIVERGENCE=1` for the red path). Fails closed without a declared
  endpoint and exits nonzero on any value mismatch or any head that served a carried
  value before its apply.
