# BAL delta-dispatch default-enablement review

> **Historical context.** The `deadline` reason and the 50 ms readiness deadline
> named in this review belong to the historical captures it cites, not to the
> current driver: a same-block caller now joins the head's live claim attempt and
> resolves terminal (settled, superseded, or abandoned), so there is no readiness
> deadline and no timed fallback. The measurements and the decision here are
> unchanged. The current lifecycle, its measurements, and its executable
> comparison are [bal-head-lifecycle-result](bal-head-lifecycle-result.md).

**Decision: NO-GO for unconditional default enablement. GO for continued
explicit opt-in on validated nodes.**

The repository's `simulation.delta_dispatch` / `DEGENBOT_DELTA_DISPATCH` default
stays `false`, and an operator who has validated a BAL-capable endpoint keeps
turning the flag on explicitly. Nothing is flipped, softened or reinterpreted
here: no production code, configuration default, telemetry or test changed, and
the operator's own `~/.config/degenbot/config.toml`
(`simulation.delta_dispatch = true`) is not the repository default and is
untouched. What the evidence still lacks is what a default-on decision needs - a
deployment compatibility decision and a measured production benefit - and both
are named below rather than inferred.

| question | answer |
| --- | --- |
| Correct? | Yes within the measured surface: 28 of 28 scripted executable parity points identical, 1,330 real candidate storage reads with 0 mismatches, 1,624 of 1,624 settled BAL writes matching cold reads. |
| Ready as an opt-in? | Yes: post-fix readiness 27 of 27, extended 73 of 74 = 98.65%, prepare p95 1.82% of the head interval, RSS inside band over 4 h 20 min with no restart. |
| Ready as the default? | No: the predeclared own-anchor band FAILS once on a real reorg, verdict parity has no matched production arm, no paired OFF/ON wall exists, and default-on would make startup fail on any node without a BAL endpoint. |
| Changed here? | No. `delta_dispatch` stays `false` (`rust/crates/foundation/degenbot-config/src/schema.rs:645`), documented as the development opt-in in `docs/rust-config-keys.md`. |

## Evidence base

Every figure below is read back from one of these reports; no figure is new. Raw
captures live under gitignored `logs/`, so the reports are the durable layer.

| report | what it owns |
| --- | --- |
| [bal-dispatch-evaluation](bal-dispatch-evaluation.md) | the mechanism, three live A/B windows, retention, the promotion criteria, the rollback |
| [bal-production-soak-result](bal-production-soak-result.md) | 60.35-minute ON soak; the same-block `deadline` failure the wait fix addresses |
| [bal-postfix-soak-result](bal-postfix-soak-result.md) | 61.53-minute ON soak after the wait fix; 27 of 27 delta, zero same-block traffic |
| [bal-matched-parity-result](bal-matched-parity-result.md) | the 28-point matched matrix, the planted control, fixed versus per-fan-out cost, live raw/JSON acquisition at one hash |
| [bal-real-read-oracle-result](bal-real-read-oracle-result.md) | real-candidate read parity through exact-anchor generations, and the one-wei plant that fails the comparator |
| [bal-extended-soak-result](bal-extended-soak-result.md) | the 4 h 20 min ON soak: readiness census, the one reorg fallback, same-block joins, prepare/RSS bands, the divergence probe |

## The committed implementation in operator terms

One flag, read once per warm-cache owner at construction; no retention knob, no
readiness knob, no second switch.

- **OFF is the shipped posture and requests nothing.** No pipeline, no
  canonical-head sink, no driver: no code path can issue a BAL request, and the
  dispatch lane is byte-identical to the pre-existing legacy path
  (`the_off_lane_is_byte_identical_to_the_shipped_legacy_path`,
  `the_opt_in_off_makes_no_bal_request_and_keeps_the_prefetch_lane`).
- **ON adds one session-scoped pipeline on the pump's accepted-header edge.** One
  BAL fetch per accepted head, off the fan-out path; a fan-out asks the driver for
  its anchor's exact `(number, hash)` and either reads the installed generation or
  takes the legacy lane with a closed-set reason. One header read per fan-out is
  the standing per-fan-out cost.
- **The source is either official EIP-7928 endpoint.** Raw
  `debug_getRawBlockAccessList` first, `eth_getBlockAccessList` only when the raw
  method is classified absent, sticky per provider session; both decode through
  one codec into one seed.
- **Startup proves the endpoint before arming anything.** A batch executor's
  construction acquires and fully decodes the current canonical head's list, so an
  endpoint serving neither representation, a `null` answer, or an undecodable list
  is a construction error naming block, hash and both methods - not a per-head
  collapse to the legacy lane.
- **Rollback is configuration plus restart.** No migration, no persisted
  generation, no file or database state: driver, generations and the eight-block
  idle window live only in session memory.

## 1. Startup, capability and deployment compatibility

Both representations serve on this node, and raw wins. At fixed anchor 26,375,043
(`0x2f139e62...3172e9`), one live reth session measured:

| representation | method | payload bytes | fetch median / max us | decode median / max us |
| --- | --- | --- | --- | --- |
| raw | `debug_getRawBlockAccessList` | 141,458 | 787 / 1,294 | 215 / 712 |
| json | `eth_getBlockAccessList` | 404,259 | 6,008 / 8,041 | 908 / 1,496 |

Raw is 2.9x smaller and roughly 7x cheaper end to end: ~1,002 us of fetch plus
decode against ~6,916 us at that anchor. Both decode into one identical seed,
entry for entry, and the negotiated production path resolves raw here. Both
post-fix soaks' startup capability probes succeeded and reported their block's
write/read counts (1,557 writes / 1,956 reads in the extended run), and that
run's 1,291 post-warm-up head preparations produced 1,291 BAL verdicts with 0
skips. The earlier production soak predates the probe; there the decode evidence
is 305 of 305 head preparations settling with no BAL skip.

The compatibility consequence is the one a default flip would create: the
capability probe is a construction-time hard failure by design. An ON session
whose node serves neither method does not degrade - it does not start. That is a
reasonable, loud contract for an operator who opted in and knows their endpoint;
it is not a posture to impose as a repository default, and this evidence contains
no deployment survey that would bound the blast radius. The compatible default
stays OFF, which requests nothing at all.

## 2. Correctness

Three evidence classes, deliberately kept apart.

**Executable, scripted, value-sensitive: 28 of 28.** Seven counts (1, 4, 16, 50,
100, 500, 1,000) x two scripted words (zero, nonzero) x two latency models x
three interleaved repetitions = 28 points, each running six legs over the same
head, candidates, deployment and scripted chain: `off-legacy`,
`on-ready-current`, `on-ready-previous`, `on-ready-unknown`, `on-cold-failed`,
`on-cold-undelivered`. Per candidate the bucket or priced figures (`gross`, `net`,
`gas`, `passed`) plus the post-execution read set are identical, as are the
failure-bucket histogram and the candidate/failure/exception tallies. The corpus
is value-sensitive: every simulated candidate executes a real SLOAD of the
executor's own slot and the receipt is a function of the stored word. A planted
one-word divergence is detected in both directions, the probe exits non-zero when
the plant is not detected or never applied, and a positive twin asserts the same
check reads identical without it.

**Real candidates, storage reads only: 1,330 comparisons, 0 mismatches.** Twelve
of the post-fix soak's own fan-out rows (6 of 246 anchors, deterministic
selection, fingerprints `0xaaad531c42181b51` / `0xb3c074d4fe39e3f9`) were
replayed through exact-anchor generations built from each row's own production
relevance set: 1,330 reads compared, **62 served**, **1,268 answered
dynamically**, 829 naturally zero words compared like any other, 16 expired
identities exercised through a supplementary two-generation leg. Every served
answer is an applied final write of its anchor's list; every answer, served or
dynamic, equals an independent cold read at the same block. The comparator's own
control (one wei added per leg) reports 19 mismatches and exits 1.

**Live content and live divergence.** Every one of the 1,624 decoded final writes
at the live anchor matched an independent cold read. In the extended soak the
armed engine-versus-RPC probe logged 73 per-fan-out summaries over 634,520 slots
with 0 divergent slots, pairs or pools - supporting evidence, since that probe
compares engine state against RPC on fallback reads and never compares the two
lanes.

What none of this is: a diverse, executable, real-candidate path corpus. Trace
rows carry read identities but no calldata, amounts or deployment state, so real
evidence is read-only; the executable side is one route shape with distinct pool
identities at scripted values and synthetic receipts. The 62-of-1,330 = 4.7%
served share is a coverage fact (a BAL seeds final writes only, and only when
relevant), not a hit-rate claim - and the dynamic-heavy majority is where a real
fan-out would spend its wall.

## 3. Readiness across the three soaks

| capture | delta share | own-anchor violations | what drove it |
| --- | --- | --- | --- |
| production soak (`3f00fac2`) | 20 of 23 = 86.96% FAIL | 3 (`deadline`) at 26,385,837 / 26,385,839 / 26,385,881 | same-block asks racing a prepare slower than the 50 ms readiness deadline; the wait fix had not landed |
| post-fix soak (`7cf487da4`) | 25 of 25 = 100%, census 27 of 27 | 0 over 24 exercised heads | no same-block traffic at all; the fixed class was unexercised |
| extended soak (`e2fa2a97a`) | 72 of 73 = 98.63%, census 73 of 74 = 98.65% PASS | 1 over 72 exercised heads FAIL | one chain reorg invalidated an exercised anchor's generation |

The extended capture is the first with real same-block traffic and the first with
a genuine readiness failure on an otherwise working lane. The single violation is
worth spelling out:

- block 26,388,359's head settled normally at 17:20:42 (`prepare_us=5,728`,
  `apply_us=95`, `held=2,710`);
- the node's websocket delivered relevant logs for that block that `eth_getLogs`
  did not return, and the pump entered its unwind path for 26,388,359;
- the next head, 26,388,360, arrived with a parent hash that did not link, so the
  delta chain cold-reset there (`parent-hash-mismatch`, 1 of 2 such heads);
- a fan-out anchoring at 26,388,359 then found no delivered head at that anchor
  and resolved `legacy` / `undelivered` **494.0 s after warm-up**, paying 2 us of
  selection wait and one prefetch batch (101 members seeded), 8 ms fan-out wall.

The cold path did what it is built to do. The band still counts it a violation, so
the gate cannot close on this capture, and this review does not rewrite the
criterion after seeing the numbers. Closing it needs either a capture without a
reorg at an exercised anchor, or a reorg-aware acceptance policy declared **before**
the capture that is judged by it. Two further read-outs: 1,217 of 1,289 settled
heads were never asked at their own anchor, so the zero-violation half is
exercised on 72 heads, not 1,289; and fan-out supply thins as the window lengthens
(33 -> 13 -> 13 -> 9 -> 5 selection rows per hour), so late-hour denominators are
single digit.

## 4. Same-block in-flight waits

| fan-out | block = anchor | wait_us | head prepare_us | head apply_us |
| --- | --- | --- | --- | --- |
| 23 | 26,388,443 | 2,174,538 | 455,782 | 48 |
| 26 | 26,388,464 | 703,910 | 133,548 | 84 |
| 33 | 26,388,501 | 671,313 | 216,846 | 33 |
| 51 | 26,390,057 | 645,862 | 151,395 | 16 |

Four of four resolved `delta`; the fourth, at 2 h 28 m, shows the join path is not
a warm-up artifact. Prior-block fan-outs paid 0-5 us (p50 1 us), so where no join
happens the fix adds no measurable dispatch latency. Two limits: four observations
cannot separate "the join always resolves delta" from "these four were lucky", and
the capture cannot decompose the 0.65-2.17 s waits into queueing, fetch, apply and
notify (`wait_us` spans the whole `wait_for_head`; `prepare_us` covers fetch plus
decode only). The path is exercised, not characterized.

## 5. Prepare cost against the head interval

| capture | prepare p50 / p90 / p95 / max (ms) | p95 ratio | max ratio |
| --- | --- | --- | --- |
| post-fix soak | 69.22 / 191.32 / 255.07 / 488.90 | 2.13% | 4.07% |
| extended soak | 55.01 / 150.50 / 217.97 / 881.31 | 1.82% | 7.34% |

Against a 12,000 ms median head interval in both captures the band (p95 <= 25%,
max below one interval) passes with a wide margin, and the interval median is 12 s
in all five extended hourly slices, so the ratio is not an artifact of a slow-block
hour. Hour 0 carries the widest p95 (321.72 ms, boot ramp plus both reorgs); the
single 881.31 ms maximum is one head in hour 3, the only prepare over 700 ms. The
same wall is the live per-head price: the negotiated prepare (fetch plus full
decode) medians 972 us at the fixed anchor.

## 6. Memory and generation growth

| fact | extended soak |
| --- | --- |
| first versus last 5-min RSS median | 4,679,068 -> 4,883,204 KiB = **+4.36%** (band <= 15%) |
| post-ramp band (from warm-up + 1 h) | 4,416,072 -> 4,883,204 KiB = **+10.58%** (band <= 15%) |
| hourly first-versus-last 5-min medians | -5.38% / +4.31% / +6.54% / -0.69% / +0.03% |
| restarts | 0 (one pid across 975 live samples) |
| `held` | max 4,381; 160 fold drops; slope -2.44/min; first-half median 2,357 vs second-half 2,079 |
| session maximum | 4,920,820 KiB, in the final minute before the stop |

Read honestly: the window supports **a plateau inside this window at this working
set** - flat at ~4.66 GiB for the final ~80 minutes, under 4.70 GiB, no restart -
not a proof of indefinite behaviour. The 4 h window is what makes even that
statement possible: the two one-hour runs could not separate the boot/state ramp
from growth (+1.77% over 60.35 min and -2.80% over 61.53 min, both starting inside
the ramp). The retention policy's own limit stands: the idle window bounds an
entry's lifetime, never the store's size, so a working set that keeps naming new
identities keeps seeding, and RSS tracks that set plus the fold horizon. Candidate
width also grew ~4x across the window, so the plateau does not extrapolate to a
wider fan-out regime.

## 7. Cost and benefit - measured, modeled, and the gap

Measured live at one fixed historical hash, one node, one session:

| term | value |
| --- | --- |
| raw fetch + decode | 787 + 215 = **~1,002 us** |
| JSON fetch + decode | 6,008 + 908 = **~6,916 us** |
| production negotiated prepare (raw-first) | median **972 us**, max 2,760 us |
| header read (every fan-out) | median 176 us |
| dynamic serial read, distinct identity | median 212 us |
| legacy prefetch batch | 862 us at 11 members, 2,387 at 35, 6,336 at 131, 21,614 at 403 |

Modeled at those live packet prices - explicitly arithmetic, never a live fan-out
claim:

| input width | legacy (header + batch(M) + 1 read) | delta covered (header) |
| --- | --- | --- |
| M = 11 | ~1,250 us | ~176 us |
| M = 131 | ~6,724 us | ~176 us |
| M = 403 | ~22,002 us | ~176 us |

The modeled saving grows with prefetch width because the legacy batch grows with
the derived identity count while a covered delta fan-out pays only the header. Two
facts keep that from becoming a production claim:

1. **No paired OFF/ON end-to-end wall exists.** The live table is arithmetic over
   measured packets, the matched matrix's walls are a synthetic latency model's
   arithmetic over the recorded wire, and every soak is ON-only. No live, matched,
   same-candidate, same-head OFF/ON fan-out comparison exists anywhere in this
   evidence, so no speedup is claimed.
2. **The real read corpus is dynamic-heavy.** 1,268 of 1,330 reads took the
   dynamic fallback (4.7% served), so on realistic shapes the delta lane pays its
   header plus most of the dynamic reads the legacy lane also pays. The live batch
   rows are also miss-shaped at that anchor, and real hits cost more per member
   than misses on this node, so even the prefetch side is a shape estimate rather
   than a working-set measurement.

The fixed per-head cost is separate and real: one BAL acquisition per accepted
head (1,294 in the extended capture), ~972 us of prepare at the fixed anchor, apply
in tens of microseconds. It is never on a fan-out's path and is never subtracted
from a fan-out wall here.

## 8. Operations

- **Dry run end to end.** No `--live`; the extended capture's 56 profitable
  candidates were all `skipped_dry_run`. No submission path, no profitable
  submission, so no economically meaningful end-to-end latency is in scope.
- **Reorgs are the readiness confounder.** Two genuine reorgs at 26,388,359 and
  26,388,581, both in hour 0, each followed by an unwind; the first is the sole
  cause of the one fallback and the one own-anchor violation. Neither is a
  delta-lane defect.
- **Log drops bound every count.** The debug-level console sink dropped 1,705,959
  records in the extended capture and lost one whole fan-out's rows (id 2); the
  dispatch census covers it (`delta`/`none`), which is why the census denominator
  is 74 and the row-level one 73. Every other row family reconciles exactly.
- **Other traffic facts.** 28 cordon rows (14 enter/exit pairs, deferrable intake,
  none attributable to a fallback), 7 header stalls of 20.4-24.6 s, 5 main-thread
  GIL warnings (startup-scale after the first), 0 RPC errors, 0 ERROR rows, 0
  restarts.
- **Endpoint negotiation is policy-inferred.** No method-level row or per-method
  metric exists, so the representation a soak used is inferred from the sticky
  raw-first policy plus a preflight probe, never capture-observed.
- **Capture identity.** The extended process ran the image it mapped at launch; a
  later test-only commit and a mid-run extension rebuild by another lane are not
  in that process, so behavior must not be derived from the current `.so`.

## 9. Promotion criteria and verdicts

Bands are the evaluation's own, predeclared before any capture; nothing below
re-anchors one after the fact. Overall: **keep OFF**.

| # | criterion | band | measured | verdict |
| --- | --- | --- | --- | --- |
| 1 | delta share of fenced fan-outs | >= 10/11 (90.91%) | extended 72 of 73 rows = 98.63%, census 73 of 74 = 98.65% | PASS |
| 2 | no settled head fails its own anchor | zero violations | 1 violation over 72 exercised anchors (71 of 72), reorg at 26,388,359 | **FAIL** |
| 3 | cold-reason census | closed labels per 1,000; `undelivered` only at the boot edge | 1 `undelivered` (13.7 per 1,000 fenced fan-outs) at 494.0 s; 2 `parent-hash-mismatch` heads (1.55 per 1,000 settled); 0 BAL skips | PASS (reported) |
| 4 | same-block in-flight class | joins resolve `delta` | 4 of 4 `delta`; waits 645,862 / 671,313 / 703,910 / 2,174,538 us | PASS (4 observations) |
| 5 | verdict and failure parity | matched OFF arm or replayed candidate set | 28 of 28 scripted points identical; 1,330 real reads 0 mismatches; no matched production arm | PASS on scripted + read-only; **UNMEASURED** on a matched executable real corpus |
| 6 | prepare p95 against head interval | <= 25% median, max below one interval | p95 217.97 ms / 12,000 ms = 1.82%; max 881.31 ms = 7.34% | PASS |
| 7 | held plateau plus RSS band | not monotonic; declared band; no restart | +4.36% overall, +10.58% post-ramp, 160 fold drops, 0 restarts | PASS in window; UNMEASURED beyond 4 h 20 min |
| 8 | fallback RPC cost | counts complete, serial read priced | 1 fallback, 1 batch (101 members seeded), read origin not logged | **UNMEASURED** |
| 9 | paired production benefit | paired OFF/ON walls on representative candidates | none exist; the live-priced table is modeled arithmetic | **UNMEASURED** |
| 10 | deployment compatibility | startup must not break on nodes without BAL | ON without a BAL endpoint is a startup construction error | **FAIL for default-on**, not for opt-in |

**Hard blockers.** Rows 2 and 10 cannot be closed by measuring more of the current
shape. Row 10 is a policy decision: a default-on flag turns every deployment whose
node serves neither BAL method into a startup failure - a breaking posture this
evidence does not justify - and the explicit-on fail-fast contract is correct for
an opt-in and wrong for a default, so any default-auto or validated-profile policy
is a separate design decision. Row 2 is a real FAIL of the band as written: the one
violation is correct cold-path behavior under a reorg, but the band says zero and
this review does not move the goalposts; a reorg-aware criterion is legitimate and
has to be declared before the capture it judges.

**Evidence limitations.** Row 5 lacks a matched arm: executable parity is scripted
(six legs per point, synthetic receipts) and real-candidate parity is read-only, so
production verdict/failure parity is UNMEASURED. Row 8's serial read occurred once
and no row records read origin. Row 9 has no paired wall at all. Row 7 is a window,
not a proof. Each is closable by measurement; none of them is a correctness
suspicion.

## Recommendation

1. **Keep the repository default OFF.** `simulation.delta_dispatch = false` is
   unchanged at `f870d0eca`; no flip is proposed or implied.
2. **Keep the explicit opt-in supported and recommended for validated nodes.**
   Operators on reth with EIP-7928 BAL RPC on a known endpoint keep the flag on,
   watching `degenbot.dispatch.storage{mode,reason}`, the per-head `bal-head` row,
   prepare walls and held/RSS. The local `simulation.delta_dispatch = true` stays
   enabled.
3. **Do not add a silent startup fallback in order to flip the default.**
   Auto-falling back on a node without a BAL endpoint would contradict the
   explicit-on fail-fast contract the startup probe enforces and would reintroduce
   silent lane switching as an unobservable default.
4. **Rollback remains configuration plus restart.** `simulation.delta_dispatch =
   false` (or `DEGENBOT_DELTA_DISPATCH=0`) and restart; no persisted generation, no
   migration, no file or database state to reverse.

### What would reopen the default question

1. **A deployment-compatibility decision.** What default-on means for an endpoint
   serving neither BAL method - a documented minimum node capability, a supported
   profile, or an explicit refusal to be default. Until then row 10 is a hard
   blocker no capture can clear.
2. **A reorg-aware readiness acceptance policy, declared before the capture that
   judges it.**
3. **Measured production benefit.** Paired OFF/ON walls over representative
   candidates, including a dynamic-heavy mix and wide fan-outs, with the fixed
   per-head cost accounted; the 4.7% served share of the real read corpus is the
   number any benefit claim must survive.
4. **Parity on a matched production arm or a replayed real candidate set**, so
   "verdicts never change" is measured rather than inferred.

Desirable but not blocking: memory evidence past 4 h 20 min and at wider fan-out
widths, and more same-block samples (4 is a path demonstration, not a distribution).

## Commit and evidence map

At `f870d0eca`, chronological:

| commit | subject | evidence it carries |
| --- | --- | --- |
| `b1788d091` | feat(simulation): add opt-in BAL delta dispatch | the flag, session driver, storage selection, the OFF-identical legacy lane |
| `b859c8839` | test(dispatch): add live BAL comparison harness | the first live A/B analysis |
| `be839880c` | feat(simulation): prepare BAL heads on canonical arrival | the canonical-head pipeline and its sink |
| `cd7150b7d` | feat(simulation): seed BAL writes from session relevance | apply-time session relevance |
| `854f94082` | feat(simulation): make BAL dispatch query-only | closed fallback labels, `bal-head` row, per-fan-out `fanout` id |
| `ce7eec496` | test(dispatch): report attributed BAL comparisons | attributed reporting |
| `efda69708` | test(dispatch): benchmark BAL prefetch scaling | the width-scaling probe |
| `72634a313` | test(dispatch): measure BAL candidate storage reads | the value-sensitive SLOAD corpus |
| `cfaf61199` | feat(simulation): bound BAL dispatch snapshot retention | the eight-block idle window, held/live/expired reporting |
| `3f00fac2c` | docs(simulation): evaluate BAL carry-forward dispatch | [bal-dispatch-evaluation](bal-dispatch-evaluation.md): mechanism, live A/B, criteria, rollback |
| `1adf0b042` | docs(simulation): report BAL production soak | [bal-production-soak-result](bal-production-soak-result.md): 20 of 23, own-anchor FAIL |
| `acdc52092` | feat(rpc): support both BAL endpoint representations | raw-first sticky negotiation, JSON fallback |
| `e4c238fa9` | feat(simulation): probe BAL capability at startup | the construction-time fail-fast probe |
| `7cf487da4` | feat(simulation): wait for in-flight BAL snapshots | the same-block join the post-fix soak could not exercise |
| `fb75cbfec` | docs(simulation): report post-fix BAL soak | [bal-postfix-soak-result](bal-postfix-soak-result.md): 27 of 27, same-block unexercised |
| `e2fa2a97a` | test(dispatch): verify matched BAL parity and costs | [bal-matched-parity-result](bal-matched-parity-result.md): 28 of 28, planted control, live acquisition |
| `af8f3551a` | test(simulation): replay real candidate reads through BAL | [bal-real-read-oracle-result](bal-real-read-oracle-result.md): 1,330 reads, 0 mismatches |
| `f870d0eca` | docs(simulation): report extended BAL soak | [bal-extended-soak-result](bal-extended-soak-result.md): 4 h 20 min capture, the reorg fallback, section 3 and 9 rows |

## Current status and open uncertainty

- Default: **OFF** (`delta_dispatch [bool] = false`, `schema.rs:645`); no flip
  landed or proposed. Local operator configuration: **ON**, unaffected.
- This change is one documentation file: no production code, test, telemetry,
  default or capture touched; nothing staged or committed by its author.
- Production verdict parity is unmeasured: every soak is ON-only, and the scripted
  matrix plus the read-only replay are the parity evidence.
- No end-to-end production speedup is measured, and none is claimed; the
  dynamic-heavy read mix (4.7% served) is why the modeled saving cannot be
  promoted. The live acquisition prices are one anchor, one node, one session, and
  their batch rows are state misses rather than a working set.
- The memory plateau is 4 h 20 min at one working set under a traffic shape whose
  candidate width grew ~4x; the readiness failure is a single event whose rate is
  not estimable from 72 exercised anchors.
- Same-block joins rest on four observations, and wait decomposition, fallback
  serial-read cost and per-fan-out header reads are not logged. Raw-versus-JSON
  negotiation is never capture-observed in a soak.
- The evidence is untracked: `logs/` is gitignored, so raw captures exist only in a
  tree that carries them; the reports are the durable layer.
