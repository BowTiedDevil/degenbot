# Matched BAL parity and endpoint costs

The controlled matched-parity, negative-control and endpoint-cost evidence for the
BAL default-enablement review: what the matched matrix proves about legacy-versus-delta
dispatch parity, what the planted divergence proves about the parity oracle itself, how
the RPC work splits into fixed per-head and per-fan-out classes, and what acquiring a
real block access list costs on live reth at one fixed historical hash.

This document decides nothing about the default. No production dispatch behavior
changed; the evidence is a review input.

## Evidence base

| artifact | kind | what it prices |
| --- | --- | --- |
| `logs/bal_matched_parity_matrix.md` | full matched matrix + negative control + wait shapes + cost attribution (`just bal-dispatch-scaling-probe`, default knobs) | dispatch parity and wire behavior on the scripted chain; synthetic latency models |
| `logs/bal_live_acquisition_result.md` | live reth probe at one fixed historical hash (`just bal-live-acquisition-probe`) | raw/JSON acquisition walls, decode seeds, real-node BAL cost, per-fan-out packet prices |
| `rust/crates/engine/degenbot-arbitrage/tests/bal_dispatch_scaling.rs` | direct pins, including the planted-divergence test | the harness's own semantics, offline |
| `logs/bal_head_lifecycle_result.md` | existing live prepare/apply lifecycle capture (cited, not re-run here) | live apply walls and readiness on real heads |

`logs/` is gitignored, so the two new captures are local evidence beside the harness
(`rust/crates/engine/degenbot-arbitrage/examples/shared/dispatch_probe.rs`,
`.../shared/dispatch_scaling.rs`, `.../bal_dispatch_scaling_probe.rs`,
`.../bal_live_acquisition_probe.rs`). Every figure below was read back from those
captures; no figure here is new.

## Matched parity coverage

The full matrix, not a narrowed slice: counts `1,4,16,50,100,500,1000` × scripted
words `zero/nonzero` × latency models `zero` and `batch 400us + 20us/member, single
400us, bal 400us` × 3 interleaved repetitions = 28 points, each point running all six
legs over the same head, candidates, deployment and scripted storage chain.

| leg | head state | lane asserted |
| --- | --- | --- |
| `off-legacy` | — (no opt-in) | `legacy` |
| `on-ready-current` | current head settled, list covering the read key | `delta` |
| `on-ready-previous` | previous head inside the read window | `delta` |
| `on-ready-unknown` | current head settled, read key deliberately omitted | `delta` |
| `on-cold-failed` | head delivered, lineage could not carry | `legacy/lineage-refused` |
| `on-cold-undelivered` | no head delivered | `legacy/undelivered` |

The parity surface is exact, not statistical: per candidate the bucket or the priced
figures (`gross`, `net`, `gas`, `passed`) **plus the post-execution read set**
(`executor:0x5eed` witnessed from every simulated candidate's access list), the
failure-bucket histogram, and the candidate/failure/exception tallies. Result: **28 of
28 points identical across every leg**; the wire census additionally pins the cold
controls' legacy batch (1 batch, members 11/35/131/403, 1 dynamic read, 0 BAL fetches)
and the ready legs' zero batch.

The three terminal shapes were forced, not assumed — each against a deliberately slow
(250 ms) preparation, with the production `wait_us` figure reported alongside the
measured wall:

| shape | lane | wait_us | selection wall us | same-block remaining wait us |
| --- | --- | --- | --- | --- |
| same-block-in-flight | `delta` | 251339 | 251345 | 251339 |
| previous-block | `delta` | 0 | 14 | — (fast path, never waits) |
| explicit-failure | `legacy/bal-unreadable` | 250306 | 250319 | — (one terminal wait, then fallback) |

So the in-flight shape joined the existing preparation and paid its actual remaining
wait, the previous anchor resolved `delta` with no wait at all, and the explicit
terminal failure waited once and fell back to the legacy lane with its real reason.
The walls here are the FORCED slow preparation's shape (250 ms), not a node claim; at
the live anchor below the same prepare takes ~1 ms median, which bounds a same-block
wait there — an inference from the measured prepare wall, not a measured wait.

Prepare and apply costs are recorded separately per settled head and never subtracted
from a fan-out wall (background and fixed-per-head tables). In the harness the settled
list scales with the corpus (it carries the derived prefetch identities), so prepare
and apply grow with the width: prepare median 3–56 us (zero model) / 1088–2136 us
(400us model), apply median 8–379 us.

## Negative control: the parity oracle can fail

The parity check is only worth trusting if it can go red. One storage word is planted
differently in the delta generation: the covered list's final write at the candidate's
read identity carries the OTHER scripted word while the chain serves the point's own,
so the delta lane must answer the planted word and the legacy lane the chain's.

| chain word | planted word | plant applied | parity | verdict rows differ |
| --- | --- | --- | --- | --- |
| zero | nonzero | yes | MISMATCH (required) | yes |
| nonzero | zero | yes | MISMATCH (required) | yes |

The divergence is exactly the value-sensitive category flip (1-wei `gas_unprofitable`
versus the word-sized `gas_profitable` receipt) over the same simulated candidates. The
control is a present oracle-correctness test, pinned twice:

- `a_planted_one_word_divergence_in_the_delta_generation_fails_the_parity_check` (the
direct test) asserts the plant reaches the settled generation and the matrix's own
parity check reports the difference;
- the probe gate exits non-zero if the plant is NOT detected or never applied — and the
positive twin (`every_leg_of_a_point_produces_the_same_candidate_verdicts`) asserts the
same check reads identical without the plant.

## Cost attribution: fixed per-head versus per-fan-out

Net RPC work per dispatch, from the wire census (a batch counted as one packet):

| lane | header reads | prefetch batches | prefetch members | dynamic serial reads | BAL fetches |
| --- | --- | --- | --- | --- | --- |
| `off-legacy`, both cold controls | 1 | 1 | M | 1 | 0 |
| `on-ready-current` / `on-ready-previous` | 1 | 0 | 0 | 0 | 0 |
| `on-ready-unknown` | 1 | 0 | 0 | 1 | 0 |
| per accepted head (background pipeline) | — | — | — | — | 1 |

`M` is the production prefetch's derived identity count at the input width: 11 (count
1), 35 (4), 131 (16), 403 (50 and above — the derivation runs over the post-cap
simulated set, `MAX_SIMULATE_CONCURRENT = 50`, so counts above 50 measure pre-filter
cost over a longer input list, not a larger simulated set).

The two classes, kept apart everywhere:

- **Fixed per-head** — one accepted canonical head's BAL work, independent of fan-out
width and never on a fan-out's path: one BAL fetch per accepted head (every measured
dispatch leg fetched zero), prepare (list fetch + full decode), apply (the one
synchronous transition).
- **Per-fan-out** — one header read at the anchor plus the lane's storage packets:
the legacy prefetch batch and its members plus the one dynamic read the derivation
cannot cover, versus the covered delta lane's zero storage packets and the unknown
control's one dynamic read. `wait_us` is 0 on every matrix leg by construction (the
generation settles before the fan-out); the nonzero waits are the forced shapes above.

Under the synthetic 400us model the mechanism's own arithmetic shows the shape of the
reduction (controlled synthetic latency, NOT a node measurement): at count 1000 / word
nonzero, `off-legacy` median 12755 us versus `on-ready-current` 814 us and
`on-ready-unknown` 1917 us, with the two cold controls back on the legacy wall
(13154/12080 us) and identical verdicts everywhere. Under the zero model the same
points are CPU-only walls (at count 1: legacy 53 us versus ready-current 31 us).

## Live reth acquisition at one fixed historical hash

Anchor: block `26375043`, hash
`0x2f139e6201003ca8e9b334a85a8069fbe3e00cdde9a8675952ca83a7e23172e9`, parent
`0x2646a43e77d62244e702686a9a32e874afb67e186917c2d626430c2c89cd1a2a` — the committed
real-coverage capture's own recorded middle block, hash verified identical to that
capture. Node: reth v2.10.1, chain 1 (local archive endpoint). Every BAL request is
hash-pinned; the storage reads are number-tagged at the anchor's number. 7 interleaved
repetitions.

| representation | method | payload bytes | fetch median us | fetch max us | decode median us | decode max us | writes | reads |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| raw | `debug_getRawBlockAccessList` | 141458 | 787 | 1294 | 215 | 712 | 1624 | 990 |
| json | `eth_getBlockAccessList` | 404259 | 6008 | 8041 | 908 | 1496 | 1624 | 990 |

Decode-seed parity: the raw decode, the JSON decode and the production negotiated
acquisition (raw-first) fold into **one identical seed, entry for entry** — 1624 final
post-block writes and 990 read-only identities — and no seed drifted between
repetitions. The production negotiation resolves the **raw** representation on this
node. Every one of the 1624 decoded final writes was compared against an independent
cold read at the anchor's number: **1624/1624 match**.

Fixed per-head BAL cost on this node: production prepare (list fetch + full decode,
negotiated) median **972 us**, max 2760 us. The raw representation is both smaller
(2.9x) and cheaper to fetch and decode than the official JSON one at this anchor.

Per-fan-out packet prices at the same anchor:

| packet | members | median us | max us |
| --- | --- | --- | --- |
| header read (every fan-out) | 1 | 176 | 301 |
| dynamic serial read, distinct identity (a real round trip) | 1 | 212 | 697 |
| dynamic serial read, one identity repeated | 1 | 0 | 203 |
| legacy prefetch batch, corpus identities | 11 | 862 | 2447 |
| legacy prefetch batch, corpus identities | 35 | 2387 | 2850 |
| legacy prefetch batch, corpus identities | 131 | 6336 | 11964 |
| legacy prefetch batch, corpus identities | 403 | 21614 | 24502 |
| real-write batch (the cold verification's own packet) | 1624 | 192103 | 192103 |

Two measurement facts sit behind those rows and are part of the result:

1. The corpus's derived identities have **no real state** at this historical anchor
(the node answers zero for each), so the corpus-identity batch rows are
member-count-shaped packets of state misses (~53 us/member at 403 members). The
real-write batch prices real hits at the anchor's own width (~118 us/member) — hits
cost more per member than misses on this node.
2. The production provider stack (`AlloyProvider`) wraps its typed calls in alloy's
default response cache: a byte-identical repeat of one storage read is answered
in-process (median 1 us) while every fresh identity pays the full serial round trip.
That asymmetry is the production stack's own behavior on the dynamic fallback, not a
node measurement.

## Modeled legacy-versus-delta at live prices — modeled, never a live fan-out claim

No live fan-out comparison exists, deliberately. The matched corpus is
value-sensitive over SCRIPTED words at synthetic identities, and its covered/unknown
controls need a generation list carrying the read key; none of that exists at a real
historical hash, and transplanting it there would mean inventing state. A live lane
pair would measure disjoint-coverage artifacts, not the lanes. The comparison below is
arithmetic — the matrix's asserted per-fan-out op counts priced at the live median
packet costs above — and is labeled modeled everywhere.

| input width | legacy (header + batch(M) + 1 read) | delta covered (header) | delta unknown (header + 1 read) |
| --- | --- | --- | --- |
| M = 11 | 176 + 862 + 212 ≈ **1250 us** | **176 us** | 176 + 212 ≈ **388 us** |
| M = 131 | 176 + 6336 + 212 ≈ **6724 us** | **176 us** | **388 us** |
| M = 403 | 176 + 21614 + 212 ≈ **22002 us** | **176 us** | **388 us** |

Plus the fixed per-head cost, width-independent and off the fan-out path: ≈ 972 us
per accepted head (live negotiated prepare). What the table does NOT say: that any
live fan-out ran, that a real candidate mix would show these walls, or that the delta
lane is faster end-to-end on a live node — the batch rows are miss-shaped at this
anchor and a real working set would carry real identities (and, per the row above,
real hits cost more per member here).

## Corpus and measurement limitations

- **One scripted chain, one route shape.** Candidate `i` routes the same two-hop
WETH -> TOKEN -> WETH shape through its own pool pair: distinct path identities, but
repeated market paths — never a diverse candidate mix. **No real diverse-candidate
parity is claimed from this corpus.**
- **Synthetic value sensitivity.** The words are scripted at a synthetic read identity
and the executor runtime is an injected reader; verdicts are receipt-shaped
(`gross`/`net`/`gas`/`passed`), not real opportunity outcomes.
- **Modeled latency.** The 400us model prices dispatched packets deterministically; it
is not a node measurement. Every modeled wall is the model's arithmetic over the
recorded wire.
- **The production cap shapes the counts.** Above 50 input candidates the simulated set
caps at 50 and the derived prefetch set saturates at 403 identities; larger counts
price the pre-filter, not a wider simulation.
- **Harness lists scale with the corpus.** The settled generation carries the derived
identities, so harness prepare/apply grow with the width; a real head's list is the
block's own content (1624 writes / 990 reads at the live anchor).
- **One anchor, one node, one session.** The live figures are one historical block on
the local reth archive endpoint with its cache in whatever state this run left it; the
fresh-versus-cold list-cache distinction is owned by the existing lifecycle capture,
not re-measured here.
- **Miss-shaped live batches.** The corpus-identity batch prices are state misses at
the anchor; only the 1624-member real-write row prices hits.
- **Provider response cache.** The repeated-read row is the alloy response cache under
`AlloyProvider`, not the node.
- **No paired live fan-out walls exist or are claimed.** The live-priced legacy-versus-
delta table is modeled arithmetic.
- **Small repetition counts, one machine.** Medians over 3 (matrix) / 7 (live)
repetitions; no tail guarantees. The captures in `logs/` are gitignored local evidence.

## What remains for the extended soak and the default review

This evidence closes the matched-parity, oracle-correctness and endpoint-cost questions
only. Still open for the parent review, unchanged here:

- the extended production soak (>= 4h after warm-up: RSS/readiness bands, same-block
coverage from real traffic or the matched in-flight substitute, abandoned/failure
census, held/live/fold behavior) — no capture here reports bytes or RSS;
- real captured candidate data for a diverse-mix parity run (the scripted corpus is a
limitation, not a substitute);
- multi-anchor live acquisition (fresh versus cold node list-cache states) if the
fixed-head cost needs a distribution rather than one anchor's point figures;
- the GO/NO-GO review itself, with its PASS/FAIL/UNMEASURED criterion table, rollback
and config behavior. **No default change is made or implied here.**

## Acceptance: met and unmet

1. **Met.** Full matched matrix (7 counts x 2 words x 2 models x 3 reps), exact
verdict/profit/failure/read-set parity asserted, the three terminal shapes forced and
gated on their lanes, prepare/apply recorded separately.
2. **Met.** Independent planted one-word negative control, both directions, with the
direct test and a probe gate that fails when the parity check does not; the positive
twin pins the check reading identical without the plant.
3. **Met.** Fixed per-head BAL + prepare/apply versus per-fan-out
header/prefetch/dynamic costs explicit in both captures and in this report; `wait_us`
and the same-block remaining wait reported beside the walls.
4. **Met under the task's own fallback branch.** Live raw/JSON acquisition at one fixed
historical hash with real BAL content: decode seeds compared and walls measured for
both representations plus the production negotiated path, and real-node BAL cost
(972 us median prepare) established. The corpus-at-anchor lane comparison was
determined infeasible without inventing state (see above), so the legacy-versus-delta
comparison is modeled and labeled, and **no live fan-out comparison was invented**.
Unmet as a measurement, by that same determination: paired live fan-out walls at a
real anchor — impossible to obtain honestly for this corpus, and never claimed.
5. **Met.** This report: parity coverage, negative control, candidate-shape limits,
net RPC operation counts, modeled and live costs, and what remains for the extended
soak and the default review.
