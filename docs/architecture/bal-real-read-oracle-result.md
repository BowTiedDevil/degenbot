# Real-candidate read-set oracle replay through BAL snapshots

The diverse-read half of the BAL correctness evidence: twelve of the post-fix soak's own
`fanout-trace` rows, replayed through exact-anchor block-access-list generations built
from each row's production relevance set, with every recorded read compared against an
independent cold read of the same block. Every answer agreed; a planted one-wei skew was
caught on every leg.

This is **real-candidate storage-read parity**, not executable path or verdict parity. A
trace row carries read identities, not calldata, amounts or deployment state, so nothing
here executes a candidate. The exact executable parity remains the scripted
value-sensitive matrix and the [matched parity result](bal-matched-parity-result.md). No
production behavior, configuration default, or telemetry changed; nothing was staged or
committed.

## Evidence base

| artifact | kind | what it pins |
| --- | --- | --- |
| `logs/bal_real_read_oracle_result.log` | live reth probe (`just bal-real-read-oracle-probe`, default knobs) | per-candidate storage-read parity over real read sets, exact-anchor value fidelity, seed-representation parity, the expiry path |
| `logs/bal_real_read_oracle_plant.log` | live reth probe with `BAL_READ_ORACLE_PLANT_WEI=1` | the comparator goes red: 19 planted one-wei differences caught, nonzero exit |
| `rust/crates/engine/degenbot-simulation/tests/bal_real_read_oracle.rs` | 13 offline tests | selection keeps each row's own production set, the comparator reads the generation's value, the one-wei plant fails it, a missing cold answer is a mismatch |
| `rust/crates/engine/degenbot-simulation/examples/shared/read_oracle.rs` | shared selection/comparison arithmetic | the module both the probe and the suite use |

`logs/` is gitignored, so the two captures are local evidence beside the harness
(`rust/crates/engine/degenbot-simulation/examples/bal_real_read_oracle_probe.rs`,
`.../examples/shared/read_oracle.rs`, `.../tests/bal_real_read_oracle.rs`). Every figure
below was read back from those captures.

## Corpus and selection

The corpus is the post-fix soak's trace stream, the first corpus whose rows carry the
production `relevance` field beside the post-read walk:

| fact | value |
| --- | --- |
| trace | `logs/bal_postfix_soak_trace.log` |
| corpus fingerprint (FNV-1a over the file bytes) | `0xaaad531c42181b51` |
| rows / anchors | 2606 / 246 |
| block range | 26387762..=26388068 |
| rows carrying the production relevance field | 2606 of 2606 |

The selection is deterministic and bounded: six anchors spread across the recorded range
(first and last included), and per anchor up to two rows chosen greedily for read-set
diversity (each pick adds the most identities the earlier picks did not carry, tie-broken
by width, candidate identity and file index). It is a sample, not a census: 12 of 2606
rows, 6 of 246 anchors.

| fact | value |
| --- | --- |
| selection fingerprint (FNV-1a over the selected rows) | `0xb3c074d4fe39e3f9` |
| anchors / rows | 6 / 12 |
| distinct `(block, cand)` candidates | 12 |
| distinct relevance addresses | 18 |
| distinct read addresses / read identities | 20 / 402 |
| rows using the recorded-read-oracle fallback | 0 |

Relevance scope is PRODUCTION for every selected row: the set the simulation seam derived
from the candidate's path and the deployment's settlement facts before the candidate ran.
No identity is invented, and the recorded read walk was never used as the relevance
filter — the filter is the row's own pre-simulation set (`read_oracle::select` keeps the
`Relevance` and its `RelevanceSource` label).

## Per-anchor replay

At each selected anchor the exact-hash canonical list was acquired through the production
negotiation (`fetch_bal_seed`, raw-first) and a fresh `DeltaSnapshotCache` generation was
built with the row's own relevance set — the production write policy: a final write to a
known slot overwrites, an unknown slot seeds only when its address is relevant, and an
irrelevant unknown write is counted and left to the dynamic path. Every recorded read was
then registered against that generation, one round drained, and each answer classified as
snapshot-served or dynamic. A separate provider batched the same identities cold at the
same block number; the code compares every identity, zeros included.

| anchor | fan-out rows | list writes | list read-only identities | selected rows | cold identities | cold batch (one capture) |
| --- | --- | --- | --- | --- | --- | --- |
| 26387762 | 37 | 1033 | 2117 | 2 | 85 | 18.48 ms |
| 26387819 | 2 | 614 | 921 | 2 | 46 | 6.61 ms |
| 26387877 | 2 | 484 | 579 | 2 | 46 | 3.09 ms |
| 26387944 | 13 | 1328 | 1901 | 2 | 77 | 12.10 ms |
| 26388006 | 2 | 536 | 678 | 2 | 46 | 3.09 ms |
| 26388068 | 10 | 411 | 768 | 2 | 308 | 24.01 ms |

`list writes` is the anchor's full list width; `applied` below is the subset whose address
is in that row's relevance set, and `skipped` is the rest — the production relevance
filter, on real lists. The lists' read-only identities are reported but not prefetched by
the production advance, so they are not a seed source.

| anchor | leg | relevance | reads | served | dynamic | natural zeros | applied / skipped writes | reads the list writes | mismatches |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 26387762 | `47367@1` | 5 | 64 | 2 | 62 | 25 | 169 / 864 | 2 | 0 |
| 26387762 | `69961@1` | 7 | 32 | 2 | 30 | 4 | 169 / 864 | 2 | 0 |
| 26387762 | `union` | 8 | 85 | 2 | 83 | 27 | 169 / 864 | 2 | 0 |
| 26387762 | `expiry` | 5 | 80 | 2 | 78 | — | 169 / 864 | 2 | 0 |
| 26387819 | `47364@1` | 5 | 44 | 5 | 39 | 24 | 142 / 472 | 5 | 0 |
| 26387819 | `268679@1` | 7 | 3 | 0 | 3 | 1 | 142 / 472 | 0 | 0 |
| 26387819 | `union` | 8 | 46 | 5 | 41 | 24 | 142 / 472 | 5 | 0 |
| 26387877 | `47364@1` | 5 | 44 | 4 | 40 | 24 | 120 / 364 | 4 | 0 |
| 26387877 | `268679@1` | 7 | 3 | 0 | 3 | 1 | 120 / 364 | 0 | 0 |
| 26387877 | `union` | 8 | 46 | 4 | 42 | 24 | 120 / 364 | 4 | 0 |
| 26387944 | `676014@1` | 7 | 53 | 6 | 47 | 23 | 179 / 1149 | 6 | 0 |
| 26387944 | `676039@1` | 7 | 43 | 6 | 37 | 13 | 179 / 1149 | 6 | 0 |
| 26387944 | `union` | 8 | 77 | 6 | 71 | 34 | 179 / 1149 | 6 | 0 |
| 26388006 | `47364@1` | 5 | 44 | 5 | 39 | 24 | 154 / 382 | 5 | 0 |
| 26388006 | `268679@1` | 7 | 3 | 0 | 3 | 1 | 154 / 382 | 0 | 0 |
| 26388006 | `union` | 8 | 46 | 5 | 41 | 24 | 154 / 382 | 5 | 0 |
| 26388068 | `269027@1` | 7 | 253 | 0 | 253 | 241 | 94 / 317 | 0 | 0 |
| 26388068 | `676500@1` | 6 | 56 | 4 | 52 | 24 | 101 / 310 | 4 | 0 |
| 26388068 | `union` | 10 | 308 | 4 | 304 | 264 | 105 / 306 | 4 | 0 |

A `union` leg groups the anchor's selected rows under the deduped union of their relevance
sets and reads their union of identities — the explicitly labelled grouped form. Only the
26388068 union changes the filter's effect (105 applied / 306 skipped instead of 101 /
310), because the union admits four more write identities than either row alone.

Totals over the 19 legs:

| class | count |
| --- | --- |
| reads compared | 1330 |
| served from the generation's snapshot | 62 |
| answered by the dynamic fallback | 1268 |
| ... of those, expired entries | 16 |
| naturally zero reads (compared like any other) | 829 |
| reads the anchor's list writes (seed overlap) | 62 |
| mismatches | **0** |

The served class is fully explained by the lists: all 62 served answers are applied final
writes of their anchor's list, and every applied write a selected row reads was served
(seed overlap = served = applied-write overlap = 62). The thin 4.7% served share is not a
regression and not a hit-rate claim: a BAL seeds only words its own block WROTE, the
production advance does not prefetch the lists' read-only identities, and most of these
candidates' reads are zero words the block did not write or addresses outside the row's
own relevance set. What this leg proves is that every answer — served or dynamic — equals
the cold word.

## Expiry path

The candidate rows' own read sets produced no naturally expired identity at the sampled
anchors (the same words are written in adjacent blocks), so expiry is exercised by one
supplementary two-generation leg at the first anchor: a `Retention::IdleBlocks(0)` cache
advances the real parent (26387761, its real parent hash) under the same relevance set,
then the real child. Sixteen identities the parent generation serves and the child neither
writes nor serves are expired at the child; every one took the dynamic fallback (none was
served from the snapshot, asserted) and every one matched its cold read at the child.

## Seed representations

At the first anchor (26387762, hash-pinned) the raw and the official JSON representations
were fetched and decoded through the production decoders under the same relevance plan,
and the production negotiation was run beside them:

| representation | writes / read-only identities |
| --- | --- |
| raw (`debug_getRawBlockAccessList`) | 169 / 102 |
| JSON (`eth_getBlockAccessList`) | 169 / 102 |
| negotiated (`fetch_bal_seed`) | 169 / 102 |

The three seeds are identical entry for entry, and the negotiation resolved **raw** on
this node — the raw preference is the provider's own and was not overridden.

## Negative control: the comparator can fail

`BAL_READ_ORACLE_PLANT_WEI=1` adds one wei to each leg's first generation-side answer
before the comparison reads it. The armed run reported **19 mismatches — exactly one per
leg — and exited 1**, naming candidate, block, identity, the answer side, the generation's
value and the cold value. Without the plant the same 19 legs report zero mismatches and
exit 0. The offline suite pins the same mechanism directly
(`a_planted_one_wei_difference_fails_the_comparison`), so a comparator that copied the
cold leg could not pass both.

## Limits and unmet acceptance

1. **Storage reads only.** Trace rows contain no calldata, amounts, or deployment state;
   no candidate executes here and no verdict, gas figure or economic outcome is claimed.
   Executable parity stays with the scripted value-sensitive matrix
   (`rust/crates/engine/degenbot-arbitrage/tests/bal_dispatch_scaling.rs`) and the
   [matched parity result](bal-matched-parity-result.md).
2. **A bounded sample.** 12 rows at 6 of 246 anchors. The selection is deterministic and
   its fingerprint pins it, but the replay is not a census of the soak's fan-outs.
3. **Live expiry is supplementary, not candidate-row-natural.** The 16 expired identities
   come from the parent generation's seeded writes under the row's relevance set, not from
   the row's own read set (which had none at these anchors). The candidate-row read
   parity itself is the exact-anchor legs above.
4. **Walls are one probe's schedule on a warm provider.** The total wall moved between
   276 ms and 379 ms across three consecutive captures, and the per-anchor cold batch
   times moved with it — which is why the walls are quoted, never claimed. No speedup,
   latency or hit-rate claim is made or implied.
5. **Read-only BAL identities are unexercised as a seed source.** The production advance
   applies final writes only; the lists' read-only census is reported (6,964 identities
   over the six anchors) but never prefetched.
6. **Gates run, and what is left to the manager.** Ran: the 13 fixture/negative-control
   pins, strict-tier clippy over the whole workspace (`just lint-rust-check`, waiver
   census included), the whole-workspace nextest suite (6150 passed, 42 skipped),
   `just fmt-check`, `just lint-markdown-check`, `just lint-comment-hygiene`, the live
   probe, and the armed negative control. NOT run: the pytest suite, the cargo/doc-test
   and standalone legs, and therefore the full `just test` — this change adds one
   example, one shared module, one test binary and docs, and no production code.

## Reproduction

```sh
# The 13 fixture pins (selection, comparator, one-wei plant, missing cold answer)
just test-rust-nextest -p degenbot-simulation -E 'binary(bal_real_read_oracle)'

# Strict-tier lint over the whole workspace, examples included
just lint-rust-check

# The live replay (default selection: 6 anchors x 2 rows)
just bal-real-read-oracle-probe

# The negative control: must exit 1 with one mismatch per leg
BAL_READ_ORACLE_PLANT_WEI=1 just bal-real-read-oracle-probe

# Wider selections, when a bigger sample is wanted
BAL_READ_ORACLE_ANCHORS=12 BAL_READ_ORACLE_ROWS=3 just bal-real-read-oracle-probe
```

Node: reth `v2.10.1-58ebce1`, chain id 1, `http://host.containers.internal:8545`.
The probe fails closed unless the machine declares that endpoint (the conftest layers,
the same resolution `test-fork` probes). Corpus fingerprint `0xaaad531c42181b51`,
selection fingerprint `0xb3c074d4fe39e3f9`.
