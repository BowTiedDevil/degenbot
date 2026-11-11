# BAL delta dispatch four-hour extended soak (opt-in ON)

One 260.01-minute live capture of the Python-hosted settlement composition at
commit `e2fa2a97a` ("test(dispatch): verify matched BAL parity and costs"), with
`simulation.delta_dispatch = true` taken from the local config and the
established dry-run owner/executor identity. The capture reached both declared
minima — 15,600.3 s (4 h 20 min 0 s) after warm-up and 1,289 settled
post-warm-up `bal-head` rows — and is a complete soak. Its verdict is **keep the
flag default OFF**: the long window exercises the fixed path for the first time
(4 same-block in-flight joins, all resolving `delta`), and every latency and
memory band passes across five hourly slices, but the readiness band as written
FAILS on one real own-anchor violation — a chain reorg that invalidated the
anchor's generation and forced the one legacy fallback — and verdict/failure
parity stays UNMEASURED without a matched OFF arm.

Facts below are read from the preserved raw capture unless a paragraph is
labelled **Reading**. No production code, configuration default, or telemetry
changed for this run.

## Verdict by predeclared band

| criterion | band | measured | verdict |
| --- | --- | --- | --- |
| capture minima | >=4 h 20 m after warm-up AND >=1000 settled post-warm-up heads | 15,600.3 s = 260.01 min; 1,289 settled heads | met |
| readiness: delta share of fenced fan-outs | >= 10/11 (90.91%) | 72 of 73 observed rows = 98.63%; census 73 of 74 = 98.65% | PASS |
| readiness: no settled head fails its own anchor | zero violations | 1 violation over 72 exercised anchors (anchor 26,388,359 served `legacy`/`undelivered` after a reorg) | **FAIL** |
| readiness: same-block anchor class | in-flight waits resolve `delta` | 4 same-block fan-outs, 4 `delta`; waits 2,174,538 / 703,910 / 671,313 / 645,862 us | PASS |
| cold-reason census | closed labels reported; `undelivered` only at the boot edge | 1 `undelivered` (13.70 per 1,000 fenced fan-outs) at 494.0 s after warm-up, on a reorg unwind; 2 `parent-hash-mismatch` heads (1.55 per 1,000 settled) | PASS (reported; one event) |
| prepare p95 vs head interval | <=25% of observed interval | 217.97 ms vs 12,000 ms = 1.82% | PASS |
| prepare max vs head interval | below one observed median interval | 881.31 ms vs 12,000 ms = 7.34% | PASS |
| memory growth (first vs last 5-min RSS median) | <=15%, no restart | +4.36%, no restart (0 pid changes in 975 live samples) | PASS |
| memory growth from the post-ramp baseline | <=15% from the warm-up+1 h median (supplementary) | +10.58% | PASS |
| memory: `held` plateaus at the fold horizon | not monotonic growth | 160 fold drops; slope -2.44 per min; first-half median 2,357 vs second-half 2,079 | PASS |
| verdict/failure parity | matched OFF arm or replayed candidate set | no matched arm in this capture | UNMEASURED |
| fallback RPC cost | counts complete; no band predeclared | 1 fallback and 1 prefetch batch over 74 fan-outs | measured (no PASS band) |
| serial-read origin / per-fan-out header reads | per-fan-out read origin | not logged by any row family | UNMEASURED |
| default ON | every criterion above passes | own-anchor band FAILS; parity unmeasured | **keep OFF** |

**Reading.** Two of the three readiness halves are strong: 73 of 74 fenced
fan-outs resolved `delta`, and the same-block class — the fix's design point —
was exercised four times without a single fallback. The FAIL is not a wait
failure: the one `legacy` row is a fan-out whose anchor's generation a real chain
reorg had just invalidated, which is exactly the case the cold path exists for.
The band's wording counts it as a violation anyway, so the promotion gate cannot
close on this capture's numbers; see "Unmet acceptance".

## Protocol and capture identity

| fact | value |
| --- | --- |
| launch | `./run_bot.sh start` (detached; no `--live`, so the driver's `dry_run` is on) |
| bot start | 2026-11-10T17:11:45-08:00 (launcher pid 2097806; driver pid 2097815) |
| warm-up end | 2026-11-10T17:12:48.681801-08:00, the first observed successful delta fan-out (`fanout=1`, `current_block=26,388,320`, `anchor=26,388,319`, `wait_us=2`) |
| warm-up neighbours | 2 `bal-head` rows precede it: anchor 26,388,318 (`cold`, `refusal="no-parent"`, the boot head) and 26,388,319 (`settled`) |
| clean stop | `./run_bot.sh stop` at 21:32:49; last log row `[runner] bot exited rc=143 2026-11-10T21:32:49-08:00` |
| soak duration after warm-up | 15,600.3 s = 260.01 min = 4 h 20 min 0 s (warm-up to the `bot exited` row) |
| bot lifetime | 2026-11-10T17:11:45 → 21:32:49 = 15,664 s = 4 h 21 min 4 s |
| settled post-warm-up heads | 1,289 (`anchor >= 26,388,320`); the conductor's own gate figure is 1,284 at its 21:32:18 checkpoint and 1,287 at the final sampler status — it subtracts the settled count read at warm-up placement (`settled_at_warmup=3`), which already included 2 heads at/after the warm-up block |
| commit | `e2fa2a97aa9083b13a1b9d6fa11fe800c48e047d`, worktree clean at preflight (0 changed paths) |
| commits landed during the run | `af8f3551a` at 17:41:55 (`test(simulation): replay real candidate reads through BAL`) — a test-only change, never in the running process |
| extension build the process runs | `src/degenbot/_ffi.abi3.so`, 324,667,353 bytes, mtime 2026-11-10T17:09:07; no source under `rust/crates`, `src/`, `examples/` newer (preflight freshness probe) |
| extension build on disk at stop | the same path was rebuilt mid-run (324,645,233 bytes, mtime 17:36) by another lane; the bot had already mapped the 17:09 image at 17:11:45, so the capture is the 17:09 build |
| owner / executor | `EXECUTOR_OWNER_ADDRESS=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266`, `DEGENBOT_INJECT_EXECUTOR_CODE=1`, `EXECUTOR_RUNTIME=/tmp/dryrun_executor_runtime.txt` (sha256 `1365bc2102a1b29d8028b845d7a67e4fddaff585c4d59fbf25937154c3ea7339`, 37,499 bytes) |
| executor image loaded | `{bytes=18748, path=/tmp/dryrun_executor_runtime.txt}` |
| flag / logging | `simulation.delta_dispatch = true` from `~/.config/degenbot/config.toml` (line 84); no `DEGENBOT_DELTA_DISPATCH` override; `DEGENBOT_DEBUG=1 RUST_LOG=degenbot=debug,degenbot::sim=trace` |
| node | `reth/v2.10.1-58ebce1/x86_64-unknown-linux-gnu`, chain id `0x1`, `http://host.containers.internal:8545` / `ws://host.containers.internal:8546` |
| resolved RPC (bot row) | `http=ws://host.containers.internal:8546 ws=ws://host.containers.internal:8546` |
| backfill | `{seed=26379518, ws_block=26388316}` (8,798 blocks) |
| connector index | `{edges=540366}` |
| block range | heads 26,388,318..26,389,610; post-warm-up 26,388,320..26,389,610 — 1,291 rows, one per block, no gap |
| disk at start | 515 G available of 1.9 T (the capture itself is 2.47 GB) |
| end-of-run metric scrape | `logs/bal_extended_soak_metrics_end.txt`, 109,830 bytes, taken 21:32:48.916 while the bot was still live |
| final process state | bot exited rc=143; sampler stopped after 15,661 s; no soak process remains |

## Startup negotiation and the raw-endpoint caveat

The startup capability probe succeeded before anything was armed:

```
INFO [sim] the BAL startup capability probe decoded the requested block's block
access list {block=26388317,
hash=0xeb81f40da8d546b5ff7f2a88d39a5a61ef2ea016ae14ce1daa0b980a0baeb28b,
seed_writes=1557, seed_reads=1956}
```

Representation negotiation is the provider's sticky raw-first pair
(`AlloyProvider::get_block_access_list`): `debug_getRawBlockAccessList` first,
`eth_getBlockAccessList` only when the raw method is classified absent. The
preflight endpoint probe (2026-11-10T17:09:36-08:00, block `0x192a74f` =
26,388,303, hash `0xb9db070e1da0bfb1751e1d9d6a5158da97418bb1d1d394ead1a87360b5126b10`)
shows this node serves **both** representations — the raw answer is a 246,228
byte envelope carrying RLP hex (`0xfa01e0d3da94…`), the JSON answer a 377,423
byte envelope carrying the official array. With raw served, the sticky
negotiation stays on **raw** and issues no JSON request.

**Reading (the caveat).** No method-level row and no per-method metric exists:
`getRawBlockAccessList` and `getBlockAccessList` appear nowhere in the
11,533,003-line capture. The representation this run used is therefore
policy-inferred plus endpoint-probed, never capture-observed. What the capture
does prove is that whichever representation was chosen decoded: the capability
probe reports 1,557 writes / 1,956 reads for its block, and 1,291 of 1,291
post-warm-up head preparations produced a BAL verdict (1,289 carried, 2 cold by
lineage refusal, **0** by BAL skip).

## Raw evidence paths

| artifact | path |
| --- | --- |
| preserved console capture | `logs/bal_extended_soak_on.log` (2,472,412,454 bytes, 11,533,003 lines) |
| `fanout-trace` extract | `logs/bal_extended_soak_trace.log` (13,290 rows, 0 mismatches) |
| 15-second RSS samples | `logs/bal_extended_soak_rss.csv` (980 rows) |
| wall stamps for the timestamp-less row families | `logs/bal_extended_soak_events.jsonl` (1,439 rows: 1,293 heads, 73 selections, 73 `cands=`) |
| end-of-run metric scrape, taken while the bot was live | `logs/bal_extended_soak_metrics_end.txt` |
| machine-readable analysis | `logs/bal_extended_soak_analysis.json` |
| conductor (launch, minima gate, finalize, preserve) | `logs/bal_extended_soak_conductor.py` |
| sampler source | `logs/bal_extended_soak_monitor.py` |
| divergence guard source | `logs/bal_extended_soak_guard.py` |
| checkpoint helper | `logs/bal_extended_soak_check.sh` |
| coarse 20-minute checkpoints | `logs/bal_extended_soak_coarse.log` (14 rows) |
| analyzer source | `logs/bal_extended_soak_analysis.py` |
| pre-flight identity record | `logs/bal_extended_soak_preflight.txt` |
| launch record | `logs/bal_extended_soak_launch.log` |
| conductor final record | `/tmp/bal_extended_soak_conductor.json` |

`logs/` is gitignored, so these are local evidence, not tracked artifacts.

## Capture integrity

| check | result |
| --- | --- |
| capture bytes / lines | 2,472,412,454 / 11,533,003 |
| trace extract rows / mismatches | 13,290 / 0 (`rg --no-filename 'fanout-trace '`) |
| `bal-head` rows | 1,293 total; 1,291 post-warm-up (1,289 `settled`, 2 `cold`) |
| pre-warm-up heads | 2: one `cold` boot head (`refusal="no-parent"`), one `settled` |
| dispatched fan-outs (census) | 74, ids 1..74: 73 `mode=delta, reason=none` + 1 `mode=legacy, reason=undelivered` |
| surviving selection rows | 73; fan-out id 2 lost **both** its selection row and its `cands=` row to the console sink |
| `cands=` rows | 73 (ids 1, 3..74) |
| `storage prefetch` rows | 1 (the one fallback) |
| retired-memo rows | 982 (hits 1,524,388, misses 507,443) |
| cordon rows | 28 (14 `ENTER` / 14 `EXIT` pairs) |
| chain reorg rows | 2 `chain reorg detected` + 10 `reorg continues` |
| header-stall diagnostic rows | 7 |
| process restarts | 0 (pid 2097815 in every one of the 975 live post-warm-up RSS samples; `pid_changes` never exceeds 0) |
| RSS sampler cadence | 980 rows, max gap 16.1 s, 0 gaps over 30 s |
| observed blocks (scrape) | `degenbot_blocks_observed_total` 1,296 against 1,293 head rows: the probe block 26,388,317, the contiguous head range, and the two blocks the reorg rewinds re-observed |
| ERROR rows / RPC errors / provider errors | 0 / 0 / 0 |
| WARN rows | 63: 23 pump hold notices, 14 `cordon ENTER`, 10 `reorg continues`, 7 header stalls, 5 `GIL held`, 2 `reorg detected`, 2 `WS delivered relevant logs not present in eth_getLogs` |
| divergence probe | 73 per-fan-out summaries, 634,520 slots compared, 0 divergent slots, 0 divergent pairs, 0 divergent pools, 0 `[sim-divergence] pool=` rows |
| console records dropped | `degenbot_log_dropped_total_total{sink="console"}` 1,705,959 at the end-of-run scrape |

The drop count matters for what the capture can prove. The console sink is a
bounded drop-counting queue fed at debug level, so every row count is a lower
bound. Here the loss is one whole fan-out (id 2) rather than a scattering of
fields: the dispatch census metric covers it (`delta`/`none`), which makes the
readiness census complete at 74 while the row-level denominator is 73.

## Readiness

Excluding warm-up, one selection row per observed fenced fan-out, plus the
census metric over all 74:

| lane | fan-outs (rows) | share | census incl. the dropped row |
| --- | --- | --- | --- |
| `delta` | 72 | 98.63% | 73 of 74 = 98.65% |
| `legacy` | 1 | 1.37% | 1 (`undelivered`) |
| total | 73 | 100% | 74 |

The band requires 90.91%; the measured share passes on both denominators.

### Anchor class

| fan-out anchor | fan-outs | lanes | cold reasons |
| --- | --- | --- | --- |
| anchor at the block being asked (`anchor == current_block`) | 4 | `delta` 4 | none |
| anchor at a previous head (`anchor < current_block`) | 69 | `delta` 68, `legacy` 1 | `undelivered` 1 |

This is the first capture in the series with non-zero same-block traffic: the
pre-fix window had 3 same-block asks that all fell to `legacy`/`deadline`, and
the one-hour post-fix window had none. Here 4 of 4 same-block asks resolved
`delta`.

### Cold-reason census

| population | label | count | per 1,000 |
| --- | --- | --- | --- |
| settled heads (n=1,289) | no cold reason (`refusal=null`, `bal=null`) | 1,289 | 0.0 |
| post-warm-up heads (n=1,291) | `parent-hash-mismatch` refusal | 2 | 1.55 per 1,000 settled |
| post-warm-up heads (n=1,291) | `no-parent` / `not-consecutive` / `parent-hash-unknown` | 0 | 0.0 |
| post-warm-up heads (n=1,291) | any BAL skip (`no-anchor-hash`, `rpc`, `decode`) | 0 | 0.0 |
| fenced fan-outs (n=73) | `delta`, reason `None` | 72 | 986.3 |
| fenced fan-outs (n=73) | `undelivered` | 1 | 13.7 |
| fenced fan-outs (n=73) | `abandoned` / `superseded` / `lineage-refused` / `bal-unreadable` / `settled` / `lineage-unreadable` | 0 | 0.0 |

No head carried a BAL skip in 4 h 20 m. The only cold labels in the whole
capture are two lineage refusals caused by reorgs, and the one fan-out that paid
for them.

### The one undelivered, verified end to end

| fact | value |
| --- | --- |
| row | `fan-out storage selection {current_block=26388360, anchor=26388359, anchor_fenced=true, storage="legacy", reason=Some("undelivered"), wait_us=2, fanout=11}` |
| wall stamp | 2026-11-10T17:21:02.673-08:00 = **494.0 s after warm-up** (8 min 14 s into the window) |
| console position | line 1,902,674 — 4,208 lines after the reorg row, 127 lines before its own prefetch row |
| cause row | `WARN [pump] BlockPump: WS delivered relevant logs not present in eth_getLogs {block=26388359, extras=[144, 60, 66, 100, 122, 119, 76, 86]}` at line 1,900,677, then `WARN [pump] BlockPump: chain reorg detected (removed log) — entering unwind path {reorg_block=26388359}` at line 1,898,466 |
| the head it asked about | anchor 26,388,359 settled at 17:20:42 (`prepare_us=5,728`, `apply_us=95`, `held=2,710`) |
| the head at the ask's own number | 26,388,360 is `cold` with `refusal="parent-hash-mismatch"` at the same sampler stamp; its `held` resets the chain from 2,710 to 354 |
| what the fallback cost | `wait_us=2`, one prefetch batch (`derived=11, learned=80, hook=10, requested=101, seeded=101`), 1 candidate, fan-out wall 8 ms |

**Reading.** The single fallback in 4 h 20 m is the reorg path, not the wait
path. The node's websocket delivered logs for block 26,388,359 that
`eth_getLogs` did not return, the pump unwound that block, and the next head
(26,388,360) arrived with a parent hash that did not link — so the delta chain
cold-reset at 26,388,360, and a fan-out anchoring at 26,388,359 found no
delivered head there. `undelivered` is the closed label for exactly that ("a head
that never arrived at all"), the fan-out paid 2 us of selection wait, and the
legacy lane ran one prefetch batch. The occurrence is inside the boot edge's
first ten minutes, matching the predeclared expectation that `undelivered`
appears only at the boot edge — though the mechanism here is a reorg, not boot.

## Wait

Raw `wait_us` from the 73 post-warm-up selection rows (`rows_without_wait_field:
0`):

| cell | n | p50 | p90 | p95 | max | mean | total |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `delta` / same-block | 4 | 703,910 | 2,174,538 | 2,174,538 | 2,174,538 | 1,048,906 | 4,195,623 us |
| `delta` / prior-block | 68 | 1 | 2 | 3 | 5 | 1.27 | 86 us |
| `legacy` / same-block | 0 | — | — | — | — | — | — |
| `legacy` / prior-block | 1 | 2 | 2 | 2 | 2 | 2.0 | 2 us |
| all fenced | 73 | 1 | 3 | 5 | 2,174,538 | 57,475 | 4,195,711 us |

Every same-block fan-out, with the head it joined:

| fanout | block = anchor | wait_us | head `prepare_us` | head `apply_us` | head `writes` | offset after warm-up |
| --- | --- | --- | --- | --- | --- | --- |
| 23 | 26,388,443 | 2,174,538 (2.17 s) | 455,782 | 48 | 320 | 1,492.5 s (24.9 min) |
| 26 | 26,388,464 | 703,910 (0.70 s) | 133,548 | 84 | 771 | 1,744.5 s (29.1 min) |
| 33 | 26,388,501 | 671,313 (0.67 s) | 216,846 | 33 | 435 | 2,186.5 s (36.4 min) |
| 51 | 26,390,057 | 645,862 (0.65 s) | 151,395 | 16 | 439 | 8,882.8 s (148.0 min) |

**Reading.** This is the first evidence that the fix does what it was built for:
four fan-outs asked the block being prepared, joined the in-flight preparation,
and resolved `delta` after waiting. Three of the four landed in the first 37
minutes; the fourth, at 2 h 28 m, shows the join path is not a warm-up artifact.
The prior-block cell is the synchronous-view path and pays 0–5 us, so the fix
adds no measurable dispatch latency where no join happens.

The wait figure and the head's own wall are not decomposable from this capture.
`wait_us` is measured around the selection's whole `wait_for_head(anchor)` span
(selection start to the head's terminal result), while `prepare_us` is only the
head's BAL fetch and full decode and `apply_us` only the apply transition — so
the waits are 3.1x to 5.3x the head's prepare wall because they also cover the
time the fan-out waited before the fetch began, plus apply and notify. No row
records that split: **UNMEASURED**. The sampler stamps rows at a 2-second scan
granularity, so the events file cannot order a head row against a selection row
inside one scan either (all four pairs land in one batch, head row first).

## Prepare wall

Raw `prepare_us` from the 1,291 post-warm-up `bal-head` rows, and head intervals
from the wall stamps of consecutive observed head rows:

| wall | n | p50 | p90 | p95 | max | mean |
| --- | --- | --- | --- | --- | --- | --- |
| prepare (ms) | 1,291 | 55.01 | 150.50 | 217.97 | 881.31 | 77.44 |
| head interval (s) | 1,290 | 12.000 | 14.002 | 14.007 | 26.006 | 12.085 |

| ratio | value | band |
| --- | --- | --- |
| p95 prepare / median head interval | 1.82% | <=25% PASS |
| max prepare / median head interval | 7.34% | < 1 interval PASS |

Per hour:

| hour | minutes | head rows | prepare p50 / p95 / max (ms) | interval p50 / max (s) | p95 ratio | max ratio |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | 60.0 | 297 | 50.42 / 321.72 / 674.43 | 12.001 / 24.0 | 2.68% | 5.62% |
| 1 | 60.0 | 300 | 57.76 / 188.46 / 486.79 | 12.000 / 16.0 | 1.57% | 4.06% |
| 2 | 60.0 | 298 | 75.46 / 211.68 / 518.31 | 12.000 / 26.0 | 1.76% | 4.32% |
| 3 | 60.0 | 296 | 48.29 / 166.24 / 881.31 | 12.000 / 26.0 | 1.39% | 7.34% |
| 4 | 20.0 | 100 | 50.26 / 115.13 / 202.89 | 12.000 / 16.0 | 0.96% | 1.69% |

**Reading.** Hour 0 carries the widest p95 (321.72 ms) because it carries the
boot ramp and both reorgs; the single 881.31 ms maximum is one head in hour 3 and
is the only prepare over 700 ms in the window. The interval distribution is
stable at a 12 s median every hour, so the ratio bands are not an artifact of a
slow-block hour. The 26 s interval maxima coincide with the 7 header stalls
(20.4–24.6 s of header silence, see confounders).

## Memory

RSS is sampled every 15 s from the driver process (pid 2097815): 980 CSV rows,
975 with a live process after warm-up.

### Overall band

| fact | value |
| --- | --- |
| first post-warm-up 5-min median | 4,679,068 KiB (4.46 GiB), 19 samples |
| last 5-min median (before stop) | 4,883,204 KiB (4.66 GiB), 19 samples |
| growth | **+4.36%** (band <=15%) PASS |
| linear slope over the window | +2.38 MiB/min (dominated by the boot ramp's first samples) |
| baseline at warm-up | 1,962,244 KiB (1.87 GiB), the ramp still climbing |
| min / max post-warm-up | 1,962,244 / 4,920,820 KiB |
| end-of-run `degenbot_process_rss_bytes` | 5,038,755,840 B (4,920,660 KiB) at 21:32:48 |
| process restarts | 0 |
| post-ramp band (from warm-up + 1 h) | 4,416,072 → 4,883,204 KiB = **+10.58%**, slope +2.50 MiB/min over 750 samples |

### Hourly RSS

Every hour reports the same statistic as the overall band (first 5-minute median
against last 5-minute median), computed inside the hour:

| hour | minutes | samples | first 5-min median (KiB) | last 5-min median (KiB) | growth | slope (MiB/min) | min / max (KiB) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 0 | 60.0 | 225 | 4,679,068 | 4,427,232 | -5.38% | -1.18 | 1,962,244 / 4,824,928 |
| 1 | 60.0 | 225 | 4,416,072 | 4,606,444 | +4.31% | +4.44 | 4,401,484 / 4,628,932 |
| 2 | 60.0 | 225 | 4,606,864 | 4,908,124 | +6.54% | +5.80 | 4,595,604 / 4,908,952 |
| 3 | 60.0 | 225 | 4,909,068 | 4,875,064 | -0.69% | -0.63 | 4,839,256 / 4,909,872 |
| 4 (partial) | 20.0 | 75 | 4,881,776 | 4,883,204 | +0.03% | +0.72 | 4,878,596 / 4,920,820 |

No hour exceeds the 15% band. Hour 0's negative growth is the shape of the boot
ramp: RSS peaks at 4,824,928 KiB (4.60 GiB) inside the first hour, settles to
4,427,232 KiB (4.22 GiB) by the hour's end, and never returns to the 1.87 GiB
warm-up baseline.

### Does the extended window support a plateau?

**Yes — inside this window, at a stepped working set.** The one-hour runs could
not separate the state-load ramp from growth: one reported +1.77% over 60.35 min
and the other -2.80% over 61.53 min, both starting inside the ramp. The 4 h 20 m
window shows the ramp ending and the traffic-independent working set settling:

- RSS rises from the 4.21 GiB hour-1 baseline to 4.68 GiB by the end of hour 2
  (+11.1% across two hours), which is the only sustained climb in the capture;
- hours 3 and 4 are flat (4,909,068 → 4,875,064 → 4,883,204 KiB; the last two
  are the hour-3 and hour-4 closing medians, -0.69% and +0.03%), i.e. **~80
  minutes of plateau at ~4.66 GiB**;
- the session maximum (4,920,820 KiB = 4.69 GiB) occurs in the final minute
  before the stop, so there is no post-peak decay evidence either;
- the post-ramp band over the last 3 h 20 m reads +10.58%, inside the 15% band
  but the largest memory number in the capture;
- the working set is bounded by traffic shape, not by the window: `held` never
  exceeds 4,381 identities and every hour has 13–37 fold drops.

So the supported claim is "flat after the second hour, under 4.70 GiB, no
restart", not "flat from warm-up". It says nothing about behaviour beyond
4 h 20 m, and an operator band is best written against the post-ramp baseline
(where this run measures +10.58%) rather than against warm-up (where the boot
ramp conceals the step).

### Generation identities

| identity | first | p50 | max | last | nonzero rows |
| --- | --- | --- | --- | --- | --- |
| `held` | 380 | 2,204 | 4,381 | 1,928 | 1,291 |
| `live` | 380 | 1,523 | 3,259 | 1,283 | 1,291 |
| `expired` | 0 | 654 | 2,667 | 645 | 1,266 |

Fitted `held` slope -2.44 identities per minute; first-half median 2,357 against
second-half 2,079; last-20-minute median 2,163; 160 fold drops, largest single
drop 2,540 identities. `held` is never monotonic in any hour.

| hour | `held` median / max | `live` median / max | `expired` median / max | fold drops | largest drop |
| --- | --- | --- | --- | --- | --- |
| 0 | 2,556 / 4,381 | 1,760 / 3,259 | 704 / 2,667 | 36 | 2,540 |
| 1 | 2,252 / 3,324 | 1,562 / 2,248 | 663 / 1,753 | 37 | 1,598 |
| 2 | 2,114 / 3,142 | 1,460 / 1,917 | 666 / 1,580 | 37 | 1,402 |
| 3 | 2,044 / 2,866 | 1,424 / 1,856 | 624 / 1,314 | 37 | 1,145 |
| 4 | 2,163 / 2,949 | 1,516 / 1,796 | 636 / 1,281 | 13 | 1,185 |

**Reading.** `held` decays rather than grows over the window (2,556 → 2,163
median across hourly slices) while RSS steps up — identity counts are entries,
never bytes, and the two series are not proxies for each other. Both reorg
rewinds land in hour 0, where `held` takes its largest resets (the largest single
drop in the capture is 2,540 identities).

## Verdict and failure parity - UNMEASURED

This capture has no OFF arm, no matched head set, and no replayed candidate set,
so it cannot compare verdicts, failure buckets, or per-candidate read sets
between lanes. **PARITY UNMEASURED.** Nothing in the capture signals a storage
divergence — no desync, no quarantine, no verify-mismatch, no reorg-driven
divergence row — and the armed engine-vs-RPC divergence probe logged 73
per-fan-out summaries over 634,520 compared slots with zero divergent slots,
zero divergent pairs, zero divergent pools and zero per-mismatch
`[sim-divergence] pool=` rows. That probe compares the engine's packed state
against RPC-served values on fallback reads during armed re-simulations; it does
not compare the two lanes, so a clean probe tally is supporting evidence, not a
parity measurement.

Descriptive comparison against the prior OFF window
(`logs/bal_dispatch_v2_off.log`, a different, earlier window on a different
build era, with zero shared heads):

| fact | prior OFF | this ON |
| --- | --- | --- |
| trace rows | 399 | 13,290 |
| trace verdicts | `none` 399 | `none` 13,259, `result` 31 |
| fenced fan-out lanes | `legacy` 9 | `delta` 72, `legacy` 1 |
| `bal-head` rows | 0 (predates the row) | 1,293 |
| drained batches / candidates | 515 / 603 | 13,700 / 13,911 |
| ok / failed / exceptions | 0 / 603 / 0 | 0 / 13,911 / 0 |
| failure buckets | `CurrencyNotSettled` 302, `ERC20: transfer amount exceeds balance` 183, `UniswapV2: TRANSFER_FAILED` 43, `TF` 38, `PoolNotInitialized` 27, `UniswapV2: INSUFFICIENT_LIQUIDITY` 10 | `CurrencyNotSettled` 7,158, `ERC20: transfer amount exceeds balance` 4,561, `UniswapV2: TRANSFER_FAILED` 668, `TF` 662, `PoolNotInitialized` 464, `UniswapV2: INSUFFICIENT_LIQUIDITY` 393, `empty` 5 |

The two windows simulated different candidate sets at different heads; this
table describes two windows and is never a lane comparison.

## Fallback RPC cost and fixed BAL cost

| term | value |
| --- | --- |
| legacy fan-outs | 1 of 74 fenced fan-outs (1.35%; census denominator) |
| its prefetch batch | 1 batch, `requested=101`, `seeded=101`, `derived=11`, `learned=80`, `hook=10` |
| its wait | 2 us of selection wait |
| its fan-out wall | 8 ms (`cands=1`, 26 warm basic hits) |
| BAL requests per accepted head | 1 by construction: 1,291 post-warm-up head rows + 2 pre-warm-up + 1 startup capability probe = **1,294 BAL acquisitions, 0 skips, 0 unreadable** |
| cold-storage aggregates over the 73 `cands=` rows | `b_fast=0`, `b_slow=307` (2,606 ms), `code=0`, `c_ms=0`, `s_fast=20,610`, `s_slow=4,745` (9,220 ms), `bh=0`, `warm_basic_hits=5,288`, `slow_ms=11,840` |
| header reads per fan-out | one by construction; no row or metric counts it — UNMEASURED |
| serial read per fallback | read origin is not logged — UNMEASURED |

**Reading.** The fallback rate is 1.35%, so the term the earlier soaks could not
price (the serial read inside a fallback) occurred once and remains unpriced;
with one event it cannot move the aggregate either way. The fixed cost — one BAL
fetch per accepted head, one header read per fan-out — is the flag's standing
overhead and no end-to-end speedup is claimed anywhere in this report.

## Candidate density, fan-out wall, verdicts

| hour | selection rows | selection width p50 / max | `cands=` candidates / sims | fan-out wall p50 / p95 / max (ms) |
| --- | --- | --- | --- | --- |
| 0 | 33 | 18 / 34 | 151 / 321 | 11 / 722 / 2,205 |
| 1 | 13 | 41 / 47 | 36 / 36 | 13 / 554 / 569 |
| 2 | 13 | 54 / 60 | 23 / 23 | 10 / 27 / 658 |
| 3 | 9 | 65 / 69 | 15 / 15 | 10 / 555 / 555 |
| 4 | 5 | 72 / 74 | 12 / 12 | 19 / 547 / 547 |
| window | 73 | 41 / 74 | 237 / 407 | 10 / 658 / 2,205 |

| fact | value |
| --- | --- |
| `fanout-trace` rows post-warm-up | 13,290 (10.29 per head) |
| attributed to a fenced fan-out / unattributed | 264 / 13,026 |
| delta fan-out wall (`fanout_ms`, n=72) | p50 10, p90 555, p95 658, max 2,205, mean 187.4 ms |
| legacy fan-out wall (n=1) | 8 ms |
| trace verdicts | `none` 13,259, `result` 31 |
| drained batches / candidates | 13,700 / 13,911; ok 0, failed 13,911, exceptions 0 |
| candidates found (scrape) | `arm="fanout"` 285, `arm="inline"` 13,682 |
| simulate verdicts (scrape) | not_profitable 13,911, profitable 56 |
| submit outcomes (scrape) | `skipped_dry_run` 56 |
| retired-memo rows | 982; hits 1,524,388, misses 507,443 |

**Reading.** Fan-out supply thins as the window lengthens (33 → 13 → 13 → 9 → 5
rows per hour) while each fan-out widens (width p50 18 → 72), so the hourly
readiness denominators shrink to single digits late in the run. The 2,205 ms
maximum is one hour-0 fan-out; the window p50 is 10 ms. All 56 profitable
candidates were `skipped_dry_run` — the capture is a dry run end to end, so no
submission path, and no end-to-end latency claim, is in scope.

## Confounders and limits

1. **Console drops bound row counts.** 1,705,959 records were dropped by the
   debug-level console queue. Fan-out id 2 lost its selection row *and* its
   `cands=` row; the dispatch census (74) covers its lane. Every other row
   family reconciles exactly between the console and the sampler's events file
   (1,293 heads, 73 selection rows, 73 `cands=` rows on both sides).
2. **Two genuine reorgs, both in hour 0.** `WS delivered relevant logs not
   present in eth_getLogs` at blocks 26,388,359 and 26,388,581, each followed by
   an unwind (`reorg_windows_total` 2, `reorg_recovery_dropped_total` 35). The
   first is the sole cause of the one `undelivered` fallback and of the single
   own-anchor violation; the second produced a cold head with no fan-out asking
   at its anchor. Neither is a delta-lane defect, but both are traffic facts that
   shape the readiness census.
3. **Header stalls.** 7 `HEADER STALL` diagnostics, 20.4–24.6 s of header
   silence (blocks 26,388,589 through 26,389,480). They explain the 26 s interval
   maxima; a fan-out whose ask landed inside one would have paid an in-flight
   wait against a preparation that could not start until the next header.
4. **Deferrable-intake cordons.** 14 `cordon ENTER` / `cordon EXIT` pairs
   (`EventBurst`, 2–288,245 events; the 288,245 one is the startup backfill), all
   released after the 10 s clean window. They hold the deferrable intake, not the
   sim intake, and the 4 h 20 m run has no fallback attributable to a cordon.
5. **Main-thread GIL warnings.** 5 `GIL held: acquire took ms` rows (4,301 ms at
   boot; then 114, 100, 103 and 505 ms). Startup-scale only after the first.
6. **The same-block class is still rare.** 4 of 74 fan-outs. Four observations
   cannot separate "the join always resolves delta" from "these four were lucky";
   what they do establish is that the path executes and is not a deadline race.
7. **No matched OFF arm exists**, so lane-vs-lane verdict/failure parity is
   UNMEASURED, as in every prior soak.
8. **Build identity is the boot image, not the on-disk artifact.** The extension
   was rebuilt at 17:36 by another lane while the bot ran; the process kept the
   image it mapped at 17:11:45 (built 17:09:07 at commit `e2fa2a97a`, worktree
   clean). A test-only commit landed at 17:41:55. Deriving this capture's
   behaviour from the current `.so` or HEAD would be wrong.
9. **Timestamp resolution.** Console rows carry no timestamps; every wall here is
   the sampler's 2 s scan stamp, which bounds head-interval resolution and cannot
   order rows inside one scan.
10. **Raw-vs-JSON negotiation is policy-inferred plus endpoint-probed**, never
    capture-observed (no method-level row or metric exists).
11. **Memory is one process's RSS under one traffic shape.** RSS depends on
    candidate width, which grew 4x across the window; the plateau is measured at
    this working set and does not extrapolate to a wider fan-out regime.

## Reproduction

The soak is complete and the bot is stopped; these commands re-derive the
analysis from the preserved artifacts, they do not start a run.

```bash
cd /workspaces/degenbot

# 1. Machine-readable facts from the preserved capture (single ON arm; the prior
#    OFF capture is read descriptively only).
python3 logs/bal_extended_soak_analysis.py \
  --on logs/bal_extended_soak_on.log \
  --trace logs/bal_extended_soak_trace.log \
  --rss logs/bal_extended_soak_rss.csv \
  --events logs/bal_extended_soak_events.jsonl \
  --off logs/bal_dispatch_v2_off.log \
  --metrics logs/bal_extended_soak_metrics_end.txt \
  --out logs/bal_extended_soak_analysis.json

# 2. Row-family census straight from the console (independent of the analyzer).
awk '/^WARN /{msg=$0; sub(/^WARN /,"",msg); n=split(msg,a," "); print a[1], a[2], a[3], a[4], a[5]}' \
  logs/bal_extended_soak_on.log | sort | uniq -c | sort -rn

# 3. The one fallback, with the reorg that caused it, in log order.
grep -nE 'chain reorg detected|undelivered|fanout=11' logs/bal_extended_soak_on.log

# 4. The four same-block waits, with their heads' own walls.
python3 - <<'PY'
import json
rows = [json.loads(line) for line in open("logs/bal_extended_soak_events.jsonl")]
heads = {r["anchor"]: r for r in rows if r.get("kind") == "bal-head"}
for s in rows:
    if s.get("kind") == "selection" and s.get("anchor") == s.get("current_block"):
        h = heads[s["anchor"]]
        print(s["fanout"], s["anchor"], s["wait_us"], h["prepare_us"], h["apply_us"])
PY

# 5. Markdown hygiene for this report.
npx --yes markdownlint-cli2 docs/architecture/bal-extended-soak-result.md
```

The capture-time protocol, for a future re-run (do not run it for this report):

```bash
cd /workspaces/degenbot
EXECUTOR_OWNER_ADDRESS=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266 \
DEGENBOT_INJECT_EXECUTOR_CODE=1 \
EXECUTOR_RUNTIME=/tmp/dryrun_executor_runtime.txt \
DEGENBOT_DEBUG=1 \
RUST_LOG='degenbot=debug,degenbot::sim=trace' \
python3 logs/bal_extended_soak_conductor.py   # launch, minima gate, scrape, stop, preserve
```

The conductor finalizes only when both minima hold (>=4 h 20 m after warm-up and
>=1,000 settled post-warm-up heads), scrapes `http://127.0.0.1:9464/metrics`
**before** `./run_bot.sh stop` (the endpoint is an in-process thread), and
preserves the ON capture plus the `rg --no-filename 'fanout-trace '` extract.

## Unmet acceptance

- **Own-anchor readiness**: FAIL as written — 1 of 72 exercised anchors served
  the legacy lane. The cause is a real chain reorg at block 26,388,359 that
  invalidated that anchor's generation, i.e. the cold path behaving as designed,
  not a skipped header or a silent fallback. Closing the gate needs either a
  capture without a reorg at an exercised anchor or a criterion re-anchored to
  exclude reorg-invalidated anchors; the second is a docs decision, not a code
  change.
- **Verdict/failure parity**: UNMEASURED — no matched OFF arm and no replayed
  candidate set.
- **Serial-read origin in the fallback cost**: UNMEASURED — no row records it,
  and only one fallback occurred to price.
- **Per-fan-out header-read overhead**: UNMEASURED — no row records it.
- **Wait decomposition**: UNMEASURED — the 0.65–2.17 s same-block waits resolve
  to `delta` but the capture cannot split them into queueing, fetch, apply and
  notify.
- **Same-block sample size**: 4 fan-outs. The join path is exercised, not
  characterised.
- **1,217 of 1,289 settled heads were never asked at their own anchor**; the
  zero-violation half of the readiness band is exercised on 72 heads.
- **Raw-vs-JSON negotiation** is policy-inferred plus endpoint-probed, not
  capture-observed.
- **Fan-out id 2** lost both of its rows to the console sink; its lane is known
  only through the census metric.
- **Multi-hour plateau**: supported for this window at ~4.66 GiB over the final
  80 minutes, not established beyond 4 h 20 m or for a wider fan-out regime.
- **Default ON**: not supported — two criteria are open (own-anchor readiness,
  parity), and the capture is a dry run with no end-to-end measurement.
