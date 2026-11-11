# BAL delta dispatch post-fix production soak (opt-in ON)

One 61.53-minute live capture of the Python-hosted settlement composition at
commit `7cf487da4` ("feat(simulation): wait for in-flight BAL snapshots"), with
`simulation.delta_dispatch = true` taken from the local config and the
established dry-run owner/executor identity. The capture reached both declared
minima — 61.53 minutes after warm-up and 305 settled post-warm-up `bal-head`
rows — and is a complete soak. Its verdict is **keep the flag default OFF**:
every measured band passes and the earlier soak's readiness failure does not
reproduce, but the promotion gate cannot close because the fixed path (an
in-flight same-block wait) saw zero same-block fan-outs in this window and
verdict/failure parity stays UNMEASURED without a matched OFF arm.

Facts below are read from the preserved raw capture unless a paragraph is
labelled **Reading**. No production code, configuration default, or telemetry
changed for this run.

## Verdict by predeclared band

| criterion | band | measured | verdict |
| --- | --- | --- | --- |
| capture minima | >=60 min after warm-up AND >=250 settled post-warm-up heads | 61.53 min; 305 settled heads | met |
| readiness: delta share of fenced fan-outs | >= 10/11 (90.91%) | 25 of 25 observed rows = 100%; census 27 of 27 | PASS |
| readiness: no settled head fails its own anchor | zero violations | 0 violations over 24 exercised heads | PASS (measured) |
| readiness: same-block anchor class | in-flight waits resolve delta | 0 same-block fan-outs in window | UNEXERCISED |
| prepare p95 vs head interval | <=25% of observed interval | 255.07 ms vs 12,000 ms = 2.13% | PASS |
| prepare max vs head interval | below one observed median interval | 488.90 ms vs 12,000 ms = 4.07% | PASS |
| memory growth (first vs last 5-min RSS median) | <=15%, no restart | -2.80%, no restart | PASS |
| memory: `held` not monotonic | not monotonic growth | 37 fold drops; slope +1.04/min | PASS |
| verdict/failure parity | matched OFF arm or replayed candidate set | no matched arm in this capture | UNMEASURED |
| fallback RPC cost | counts complete; no band predeclared | 0 fallbacks, 0 prefetch batches/members | measured (no PASS band) |
| serial-read origin / header reads | per-fan-out read origin | not logged by any row family | UNMEASURED |
| default ON | all of the above pass | parity unmeasured; fixed path unexercised | **keep OFF** |

## Protocol and capture identity

| fact | value |
| --- | --- |
| launch | `./run_bot.sh start` (detached, `setsid`; no `--live`, so the driver's `dry_run` is on) |
| bot start | 2026-11-10T15:19:34-08:00 (pidfile 1985869 = `uv run` child; driver pid 1985880) |
| warm-up end | 2026-11-10T15:21:02.181-08:00, the first observed successful delta fan-out (`fanout=2`, `current_block=26387763`, `anchor=26387762`, `wait_us=1`) |
| warm-up, stricter bound | the dispatch census proves `fanout=1` (pre warm-up, selection row lost) was also `delta`; its `cands=` row completes 2026-11-10T15:20:58.421-08:00. Under that placement the window is 61.6 min — the minima hold either way |
| clean stop | `./run_bot.sh stop`; last log row `[runner] bot exited rc=143 2026-11-10T16:22:34-08:00` |
| soak duration after warm-up | 3,691.8 s = 61.53 min (warm-up to the `[runner] bot exited` row) |
| settled post-warm-up heads | 305, all `settled`/`carried` (4 pre-warm-up heads: 3 settled, 1 cold boot head) |
| commit | `7cf487da4993c32fd922cea703c769423fb1964b` (`feat(simulation): wait for in-flight BAL snapshots`); worktree clean at start and stop |
| extension build | `src/degenbot/_ffi.abi3.so`, 324,667,353 bytes, mtime 2026-11-10 15:11; no source under `rust/crates`, `src/`, or `examples/` is newer (only `rust/target` build byproducts) |
| owner / executor | `EXECUTOR_OWNER_ADDRESS=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266`, `DEGENBOT_INJECT_EXECUTOR_CODE=1`, `EXECUTOR_RUNTIME=/tmp/dryrun_executor_runtime.txt` (sha256 `1365bc2102a1b29d8028b845d7a67e4fddaff585c4d59fbf25937154c3ea7339`, 37,499 bytes on disk) |
| executor image loaded | `{bytes=18748, path=/tmp/dryrun_executor_runtime.txt}` |
| flag / logging | `simulation.delta_dispatch = true` from `~/.config/degenbot/config.toml` (line 84, file mtime 2026-11-10 10:19); no `DEGENBOT_DELTA_DISPATCH` override; `DEGENBOT_DEBUG=1 RUST_LOG=degenbot=debug,degenbot::sim=trace` |
| node | `reth/v2.10.1-58ebce1/x86_64-unknown-linux-gnu`, chain id `0x1`, `http://host.containers.internal:8545` / `ws://host.containers.internal:8546` |
| resolved RPC (bot row) | `http=ws://host.containers.internal:8546 ws=ws://host.containers.internal:8546` |
| backfill | `{seed=26379518, ws_block=26387757}` (8,239 blocks, 389,559 logs) |
| connector index | `{edges=540366}` |
| block range | 26387762..26388068 (capture); 26387763..26388068 post-warm-up |
| disk at start | 516 G available of 1.9 T |
| final process state | bot not running; monitor exited 0; analyzer exited 0; no soak process remains |

## Startup negotiation

The ON startup capability probe succeeded before anything was armed; a startup
failure would have been STOP/FAIL, not a warm-up delay:

```
INFO [sim] the BAL startup capability probe decoded the requested block's block
access list {block=26387758,
hash=0xe431d0ba0fafb28da0011e037e70961fceaf51497b85eb1a31703b9d56072be5,
seed_writes=1177, seed_reads=1267}
```

Representation negotiation is the provider's sticky raw-first pair
(`AlloyProvider::get_block_access_list`): `debug_getRawBlockAccessList` first,
`eth_getBlockAccessList` only when the raw method is classified absent. The
preflight endpoint probe (2026-11-10T15:18:29-08:00, block `0x192a526` =
26,387,750, hash `0xedb38a8786077f10a888542de1699780da642da42e4377d85f0c1fdd1ddd73f3`)
shows this node serves **both** representations — the raw answer is a 157,018
byte envelope carrying RLP hex (`0xfa013296…`), the JSON answer a 254,678 byte
envelope carrying the official array. With raw served, the sticky negotiation
stays on **raw** and issues no JSON request.

**Reading.** No method-level row or per-method metric exists — the capture logs
no RPC request sequence (`getRawBlockAccessList`/`getBlockAccessList` appear
nowhere in the 3,897,144-line capture), so the representation is policy-inferred
plus endpoint-probed, not capture-observed. The decode evidence says whichever
representation was chosen worked: the probe decoded 1,177 writes / 1,267 reads
and 305 of 305 head preparations settled with zero `bal` skips.

## Raw evidence paths

| artifact | path |
| --- | --- |
| preserved console capture | `logs/bal_postfix_soak_on.log` (855,263,331 bytes, 3,897,144 lines) |
| `fanout-trace` extract | `logs/bal_postfix_soak_trace.log` (2,606 rows) |
| 15-second RSS samples | `logs/bal_postfix_soak_rss.csv` (233 rows) |
| wall stamps for timestamp-less rows | `logs/bal_postfix_soak_events.jsonl` |
| end-of-run metric scrape, taken 16:22:33 while the bot was still live | `logs/bal_postfix_soak_metrics_end.txt` (105,735 bytes) |
| machine-readable analysis | `logs/bal_postfix_soak_analysis.json` |
| sampler source | `logs/bal_postfix_soak_monitor.py` (also `/tmp/bal_postfix_soak_monitor.py`) |
| analyzer source | `logs/bal_postfix_soak_analysis.py` (also `/tmp/bal_postfix_soak_analysis.py`) |
| pre-flight identity record | `logs/bal_postfix_soak_preflight.txt` (also `/tmp/bal_postfix_soak_preflight.txt`) |
| sampler end state | `/tmp/bal_postfix_soak_status.json` |

`logs/` is gitignored, so these are local evidence, not tracked artifacts. The
earlier soak's `logs/bal_production_soak_*` artifacts were not touched (mtimes
unchanged at 2026-11-10 09:1x).

## Capture integrity

| check | result |
| --- | --- |
| trace extract rows / mismatches | 2,606 / 0 (`rg --no-filename 'fanout-trace '`) |
| `bal-head` rows | 309 total; 305 post-warm-up, all `settled`/`carried` |
| pre-warm-up heads | 4: 3 `settled`, 1 `cold` boot head (`anchor=26387759`, `refusal="no-parent"`, `bal=null`) |
| dispatched fan-outs (census) | 27, ids 1..27, every one `mode=delta, reason=none` |
| surviving selection rows | 25; fan-out ids 1 and 5 lost their selection row to the console sink (`cands=` rows survive for both) |
| `cands=` rows | 27 (one per fan-out id) |
| `storage prefetch` rows | 0 |
| retired-memo rows | 255 (254 post-warm-up) |
| cordon rows | 6 (3 `ENTER` / 3 `EXIT`) |
| process restarts | 0 (driver pid 1985880 in 231 consecutive samples; one off-pid sample 1995042 at 16:22:34.298 is the stop shell transiently matching the driver argv pattern at the exit instant — the log holds exactly one `[runner] started` / `bot exited` pair) |
| reorgs | 0 `reorg` rows; `degenbot_reorg_recovery_dropped_total` reads 71 at the end-of-run scrape (the only scrape preserved) |
| RPC errors | none logged (0 rows matching rpc/provider error families) |
| ERROR rows / WARN rows | 0 / 0 |
| console records dropped | `degenbot_log_dropped_total_total{sink="console"}` 1,538,772 at the end-of-run scrape; the first post-warm-up scrape was not preserved |

The drop count matters for what the capture can prove. The console sink is a
bounded drop-counting queue fed at debug level, so every row count is a lower
bound. Two selection rows (fan-out ids 1 and 5) were dropped; the dispatch
census counter covers both — it reads 27 `delta`/`none`, matching the 27
`cands=` rows id-for-id (1..27), so the readiness census below is complete even
where row detail is lost. Candidate-level trace rows remain a lower bound
(166 of 196 joined candidates attributed to their fan-out).

## Readiness

Excluding warm-up, one selection row per observed fenced fan-out:

| lane | fan-outs (rows) | share | census incl. dropped rows |
| --- | --- | --- | --- |
| `delta` | 25 | 100% | 27 of 27 = 100% |
| `legacy` | 0 | 0% | 0 |
| total | 25 | 100% | 27 |

The band requires 90.91%; the measured share passes on both the row-level
denominator and the complete census denominator.

### Anchor class

| fan-out anchor | fan-outs | lanes | cold reason |
| --- | --- | --- | --- |
| anchor at a previous head (`anchor < current_block`) | 25 | `delta` 25 | none |
| anchor at the block being asked (`anchor == current_block`) | 0 | — | — |

Every observed fan-out anchored at a prior head and every one resolved `delta`.
The earlier soak's failing class — a same-block ask racing the head's own
prepare — saw **zero traffic** in this window. The rows do carry the new
`wait_us` field (25 of 25), so an in-flight join would have been visible; none
occurred.

### Cold-reason census

| population | label | count | per 1,000 |
| --- | --- | --- | --- |
| settled heads (n=305) | no cold reason (`refusal=null`, `bal=null`) | 305 | 0.0 cold |
| settled heads (n=305) | `lineage-refused`, `bal-unreadable`, `not-consecutive`, any BAL skip | 0 | 0.0 |
| fenced fan-outs (n=25) | `delta`, reason `None` | 25 | 1000.0 |
| fenced fan-outs (n=25) | `deadline` / `abandoned` / `superseded` / `undelivered` / `lineage-refused` / `bal-unreadable` / `settled` / `lineage-unreadable` | 0 | 0.0 |
| fenced fan-outs (n=25) | `legacy` (any reason) | 0 | 0.0 |

No head transition was cold and no fallback reason survived anywhere. No
`abandoned` event occurred, so there is no head/sequence to interpret.

### Settled heads serving their own anchor

24 of the 305 settled heads had a fenced fan-out anchored at their own number
(25 fan-outs over 24 anchors). All 25 were served `delta`; zero violations, so
the zero-violation half of the readiness band passes on the exercised
population. The remaining 281 settled heads were never asked at their own
anchor and are unmeasured for this criterion, not passing.

**Reading.** The earlier soak's readiness failure was entirely the same-block
class (3 of 3 same-block asks fell to `legacy` with `deadline`, tipping the
share under 90.91%). This window contained no same-block asks at all, so the
failure did not recur — but neither did the fix's intended behavior (a fan-out
joining an in-flight preparation and waiting for its terminal result). The
readiness PASS is real for what ran; it is not evidence that an in-flight wait
resolves `delta`, because nothing waited.

## Wait

Raw `wait_us` from the 25 post-warm-up selection rows (the field is present and
parsed on every row):

| cell | n | p50 | p90 | p95 | max | mean | total |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `delta` / same-block | 0 | — | — | — | — | — | — |
| `delta` / prior-block | 25 | 1 | 1 | 1 | 1 | 0.88 | 22 us |
| `legacy` / same-block | 0 | — | — | — | — | — | — |
| `legacy` / prior-block | 0 | — | — | — | — | — | — |

Histogram: 22 rows at 1 us, 3 rows at 0 us. Every wait resolved `delta`, so per
protocol none of them is a failure.

Correlating same-block wait with the head's own prepare wall is **not possible**
in this capture: the same-block population is empty (`same_block_rows: []` in
the analysis JSON). The prior-block waits are the synchronous-view path, one
microsecond of bookkeeping, and show the fix adds no measurable dispatch
latency where no join happens.

## Prepare wall

Raw `prepare_us` from the 305 post-warm-up `bal-head` rows, and head intervals
from the wall stamps of consecutive observed head rows:

| wall | n | p50 | p90 | p95 | max | mean |
| --- | --- | --- | --- | --- | --- | --- |
| prepare (ms) | 305 | 69.22 | 191.32 | 255.07 | 488.90 | 91.96 |
| head interval (s) | 304 | 12.000 | 14.001 | 14.007 | 24.000 | 12.119 |

| ratio | value | band |
| --- | --- | --- |
| p95 prepare / median head interval | 2.13% | <= 25% PASS |
| max prepare / median head interval | 4.07% | < 1 interval PASS |

The observed interval distribution is 8.0-24.0 s with a 12.0 s median. The
descriptive comparison to the earlier window (80.68 / 202.87 / 253.20 / 451.97
ms) is two different windows, not a regression or an improvement claim.

## Memory

RSS is sampled every 15 s from the actual driver process (`VmRSS` of
`/home/dev/.venvs/degenbot/bin/python examples/eth_settlement_arbitrage_v2_v3_v4_rust.py`,
pid 1985880): 233 CSV rows, 230 post-warm-up samples with a live process.

| fact | value |
| --- | --- |
| first post-warm-up 5-min median | 4,587,460 KiB (4.38 GiB), 19 samples |
| last 5-min median (before stop) | 4,459,056 KiB (4.25 GiB), 18 samples |
| growth | -2.80% (band <=15%) PASS |
| linear slope over the window | -4.80 MiB/min |
| baseline at warm-up | 2,318,824 KiB (boot ramp still rising) |
| min / max post-warm-up | 2,318,824 / 5,021,256 KiB (max at elapsed ~35 min) |
| end-of-run `degenbot_process_rss_bytes` | 4,605,386,752 B (4,497,448 KiB) at 16:22:33 |
| process restarts | 0 |

The band passes with a negative slope this window (the earlier soak was +1.77%
over 60.35 min at +1.24 MiB/min). The first 5-minute median sits above the
warm-up baseline because the boot-state ramp was still climbing.

### Generation identities

| identity | first | p50 | max | last |
| --- | --- | --- | --- | --- |
| `held` | 786 | 2,703 | 4,518 | 3,028 |
| `live` | 786 | 1,883 | 3,384 | 2,046 |
| `expired` | 0 | 807 | 2,704 | 982 |

`p50` here is the sample median over the 305 raw rows (the analysis JSON's
method); the prepare table uses nearest-rank. `expired` is non-zero on 300 of
305 rows. `held` does not grow monotonically: 37 fold drops occurred (largest
single drop 2,521 identities), first-half median 2,686 against second-half
2,705, last-20-minute median 2,783.5, fitted slope +1.04 identities per minute.

**Reading.** The held slope is positive this hour (+1.04/min) while RSS slopes
negative; identity counts are entries, never bytes, and the 37 fold drops keep
the series non-monotonic. One hour is not proof of a multi-hour plateau in
either direction.

## Verdict and failure parity - UNMEASURED

This capture has no OFF arm, no matched head set, and no replayed candidate set,
so it cannot compare verdicts, failure buckets, or per-candidate read sets
between lanes. **PARITY UNMEASURED.** Nothing in the capture signals a storage
divergence — no desync, no quarantine, no verify-mismatch, no reorg row — and
the armed engine-vs-RPC divergence probe logged 27 per-fan-out summaries over
39,948 compared slots with zero divergent slots, zero divergent pairs, zero
divergent pools, and zero per-mismatch `[sim-divergence] pool=` rows. That
probe compares the engine's packed state against RPC-served values on fallback
reads during armed re-simulations; it does not compare the two lanes, so a clean
probe tally is supporting evidence, not a parity measurement.

Descriptive comparison against the prior OFF window
(`logs/bal_dispatch_v2_off.log`, a different, earlier window on a different
build era, with zero shared heads):

| fact | prior OFF | this ON |
| --- | --- | --- |
| trace rows | 399 | 2,606 (2,569 post-warm-up) |
| trace verdicts | `none` 399 | `none` 2,558, `result` 11 post-warm-up |
| fenced fan-out lanes | `legacy` 9 | `delta` 25, `legacy` 0 post-warm-up |
| `bal-head` rows | 0 (predates the row) | 309 |
| drained batches / candidates | 515 / 603 | 2,535 / 2,727 |
| ok / failed / exceptions | 0 / 603 / 0 | 0 / 2,727 / 0 |
| failure buckets | `CurrencyNotSettled` 302, `ERC20: transfer amount exceeds balance` 183, `UniswapV2: TRANSFER_FAILED` 43, `TF` 38, `PoolNotInitialized` 27, `UniswapV2: INSUFFICIENT_LIQUIDITY` 10 | `CurrencyNotSettled` 1,243, `ERC20: transfer amount exceeds balance` 910, `UniswapV2: INSUFFICIENT_LIQUIDITY` 170, `UniswapV2: TRANSFER_FAILED` 151, `TF` 143, `PoolNotInitialized` 109, `empty` 1 |

The two windows simulated different candidate sets at different heads and the
bucket mixes differ; this table is a description of two windows, never a lane
comparison.

## Fallback RPC cost

| term | value |
| --- | --- |
| legacy fan-outs | 0 (earlier soak: 3) |
| legacy prefetch batches | 0 (earlier soak: 3) |
| prefetch members requested / seeded | 0 / 0 (earlier soak: 271 / 271) |
| BAL requests per accepted head | 1 by construction; 305 observed head rows = 305 requests, plus 1 startup probe = 306 acquisitions, 0 skips |
| wait cost | 22 us total across 25 fan-outs (max 1 us per fan-out) |
| header reads per fan-out | not separately logged - UNMEASURED |
| serial read per fallback | no fallback occurred; read-origin not logged - UNMEASURED |

Cold-storage aggregates over the 26 post-warm-up `cands=` rows: `b_fast=0`,
`b_slow=219` (119 ms), `code=0`, `c_ms=0`, `s_fast=20,990`, `s_slow=6,341`
(2,609 ms), `bh=0`, `warm_basic_hits=4,739`, `slow_ms=2,732`.

**Reading.** The fallback rate is zero in this window, so the fallback cost the
earlier soak could not fully price (the serial read) is not merely unpriced —
it is absent. No end-to-end speedup is claimed anywhere in this report.

## Candidate density, fan-out durations, verdicts

| fact | value |
| --- | --- |
| candidate-bearing blocks post-warm-up | 245 of 305 |
| `fanout-trace` rows post-warm-up | 2,569 (8.42 per head) |
| attributed to a fenced fan-out / unattributed | 166 / 2,403 |
| candidates per fenced fan-out (`cands=`, 26 post rows) | 197 total, mean 7.58, max 50 |
| delta fan-out wall (`fanout_ms`, n=25) | p50 9, p90 98, p95 110, max 971, mean 55.8 ms |
| trace verdicts post-warm-up | `none` 2,558, `result` 11 |
| drained batches / candidates (whole capture) | 2,535 / 2,727 |
| ok / failed / exceptions (whole capture) | 0 / 2,727 / 0 |
| retired-memo rows (post-warm-up) | 254; hits 227,834, misses 100,325 |

The 971 ms delta tail is one fan-out (`fanout=3`, 46 candidates); the p50 is
9 ms, so the wall distribution is a few wide fan-outs over many narrow ones.

## Comparison with the earlier soak

Both captures ran the same protocol on the same environment shape; the windows
share no heads and the builds differ by the wait fix, so every row is
descriptive.

| fact | earlier soak (`3f00fac2`) | this soak (`7cf487da4`) |
| --- | --- | --- |
| window after warm-up | 60.35 min | 61.53 min |
| settled post-warm-up heads | 302 | 305 |
| readiness delta share | 20 / 23 = 86.96% FAIL | 25 / 25 = 100% (census 27/27) PASS |
| same-block fan-outs | 3 (all `legacy`, `deadline`) | 0 |
| prior-block fan-outs | 20 / 20 `delta` | 25 / 25 `delta` |
| own-anchor violations | 3 | 0 |
| fallbacks per 1,000 fan-outs | 130.4 | 0.0 |
| prepare p50/p90/p95/max (ms) | 80.68 / 202.87 / 253.20 / 451.97 | 69.22 / 191.32 / 255.07 / 488.90 |
| median head interval | 12.000 s | 12.000 s |
| RSS growth (first vs last 5-min median) | +1.77% | -2.80% |
| `held` slope | -1.95 / min | +1.04 / min (37 fold drops both) |
| prefetch members | 271 | 0 |
| `fanout-trace` rows post | 2,447 | 2,569 |
| drained candidates (whole capture) | 2,803 | 2,727 |
| divergence slots compared | 91,724 (0 divergent) | 39,948 (0 divergent) |
| console records dropped (end scrape) | 1,564,399 | 1,538,772 |
| `wait_us` field | absent | present on 25 / 25 rows |

**Reading.** The two readiness halves flipped from FAIL to PASS without a single
fallback event, but the swing is driven by fan-out supply, not by observed fix
behavior: the earlier window had 3 same-block asks, this one had none. What the
fix demonstrably changed in this capture is the row surface (`wait_us` on every
selection) and the absence of any `deadline` fallback — the latter consistent
with the fix but also with a window that never raced a prepare.

## Confounders and limits

1. **Console drops bound row counts; the census bounds the drops.** 1.54 M
   records were dropped by the debug-level console queue over the capture. Two
   selection rows (fan-out ids 1, 5) were lost; the dispatch census (27) and
   the 27 `cands=` rows cover them id-for-id. Candidate-level trace attribution
   remains a lower bound (166 of 196 joined candidates).
2. **Fenced fan-out density is clumpy and market-driven.** 27 fan-outs over 306
   blocks, with 50/46/50-candidate fan-outs in the first minutes and singletons
   afterwards. The readiness denominator is 25 observed rows; the census
   denominator is 27.
3. **The same-block class — the fix's design point — is unexercised.** Zero of
   27 fan-outs asked the block being prepared. This is a measurement gap, not a
   pass, for the in-flight wait behavior.
4. **The window is one hour.** The memory band is a 60-minute first-versus-last
   comparison; `held` slopes positive this window and negative in the earlier
   one; neither proves a plateau.
5. **No matched OFF arm exists**, so lane-vs-lane verdict/failure parity is
   UNMEASURED, exactly as in the earlier soak.
6. **The prior OFF window** (`logs/bal_dispatch_v2_off.log`) is a different
   window, a different build era, and shares no head with this run.
7. **Head-interval resolution comes from the sampler's row wall stamps**, which
   carry the sampler's 2 s log-scan granularity.
8. **The one off-pid RSS sample** at the exit instant is the stop shell matching
   the driver argv pattern, not a bot restart (one start/exit pair in the log).

## Reproduction

Launch (exactly the protocol environment; detached):

```bash
cd /workspaces/degenbot
EXECUTOR_OWNER_ADDRESS=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266 \
DEGENBOT_INJECT_EXECUTOR_CODE=1 \
EXECUTOR_RUNTIME=/tmp/dryrun_executor_runtime.txt \
DEGENBOT_DEBUG=1 \
RUST_LOG='degenbot=debug,degenbot::sim=trace' \
./run_bot.sh start
```

`simulation.delta_dispatch = true` comes from `~/.config/degenbot/config.toml`;
do not pass `DEGENBOT_DELTA_DISPATCH` and do not pass `--live`.

Sampler (15 s RSS; wall stamps for the timestamp-less row families; captures the
optional `wait_us` field):

```bash
python3 /tmp/bal_postfix_soak_monitor.py
```

Scrape metrics **before** stopping the bot (the endpoint is an in-process
thread, gone when `./run_bot.sh stop` returns):

```bash
cd /workspaces/degenbot
curl -s http://127.0.0.1:9464/metrics > logs/bal_postfix_soak_metrics_end.txt
```

Then stop, preserve, and extract:

```bash
cd /workspaces/degenbot
./run_bot.sh stop
cp logs/bot_run.log logs/bal_postfix_soak_on.log
rg --no-filename 'fanout-trace ' logs/bal_postfix_soak_on.log \
  > logs/bal_postfix_soak_trace.log
```

Analyze (single ON arm; the prior OFF capture is read descriptively only):

```bash
python3 /tmp/bal_postfix_soak_analysis.py \
  --on logs/bal_postfix_soak_on.log \
  --trace logs/bal_postfix_soak_trace.log \
  --rss logs/bal_postfix_soak_rss.csv \
  --events logs/bal_postfix_soak_events.jsonl \
  --off logs/bal_dispatch_v2_off.log \
  --metrics logs/bal_postfix_soak_metrics_end.txt \
  --out logs/bal_postfix_soak_analysis.json
```

The analyzer reuses `scripts/bal_live_ab_analysis.py`'s row parsers and adds a
wait-aware selection parser (the repo parser predates `wait_us`), the divergence
summary tally, and the raw-metric census comparison.

## Unmet acceptance

- In-flight same-block wait behavior (the fix's design point): **UNEXERCISED** —
  0 same-block fan-outs in 27 dispatches; `wait_us` stats for that cell are
  empty and no wait-vs-prepare correlation exists. The `wait` criterion is
  measured PASS only for the prior-block cell (p50/p90/p95/max = 1 us).
- Verdict/failure parity: **UNMEASURED** — no matched OFF arm or replayed
  candidate set in this capture.
- Serial-read origin in the fallback cost: **UNMEASURED** — no row records it,
  and no fallback occurred to price.
- Per-fan-out header-read overhead: **UNMEASURED** — no row records it.
- 281 of 305 settled heads were never asked at their own anchor; the
  zero-violation half of the readiness band is exercised on 24 heads.
- Raw-vs-JSON negotiation is policy-inferred plus endpoint-probed, not
  capture-observed (no method-level row or metric exists).
- Fan-out ids 1 and 5 lost selection rows to the console sink; their lane is
  known only through the dispatch census (`delta`/`none`).
- Multi-hour memory plateau: **UNMEASURED** — one hour is one hour.
- Default ON: **not supported** — no band fails, but the promotion gate cannot
  close while the fixed path is unexercised and parity is unmeasured. Closing it
  needs (1) a capture whose fan-out supply includes same-block asks joining
  in-flight preparations, and (2) a matched OFF arm or a replayed candidate set
  for verdict/failure parity.
