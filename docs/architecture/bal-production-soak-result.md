# BAL delta dispatch production soak (opt-in ON)

One 60-minute live capture of the Python-hosted settlement composition with
`simulation.delta_dispatch` (the `DEGENBOT_DELTA_DISPATCH` opt-in) enabled, run
against the established dry-run owner/executor identity. The capture reached
both declared minima — 60.35 minutes after warm-up and 302 settled post-warm-up
`bal-head` rows — and is a complete soak, not an insufficient one. Its verdict
is **keep the flag default OFF**: the readiness band fails, and verdict/failure
parity is UNMEASURED because this ON-only arm has no matched OFF head set.

Facts below are read from the preserved raw capture unless a paragraph is
labelled **Reading**. No production code, configuration default, or telemetry
changed for this run.

## Verdict by predeclared band

| criterion | band | measured | verdict |
| --- | --- | --- | --- |
| capture minima | >=60 min after warm-up AND >=250 settled post-warm-up heads | 60.35 min; 302 settled heads | met |
| readiness: delta share of fenced fan-outs | >= 10/11 (90.91%) | 20 of 23 = 86.96% | **FAIL** |
| readiness: no settled head fails its own anchor | zero violations | 3 violations (`deadline`) | **FAIL** |
| prepare p95 vs head interval | <=25% of observed interval | 253.20 ms vs 12,000 ms = 2.11% | PASS |
| prepare max vs head interval | below one observed median interval | 451.97 ms vs 12,000 ms = 3.77% | PASS |
| memory growth (first vs last 5-min RSS median) | <=15%, no restart | +1.77%, no restart | PASS |
| memory: `held` plateaus at the fold horizon | not monotonic growth | slope -1.95 per min; 37 fold drops | PASS |
| verdict/failure parity | matched OFF arm or replayed candidate set | no matched arm in this capture | **UNMEASURED** |
| fallback RPC cost | counts complete; no band predeclared | 3 fallbacks, 271 prefetch members | measured (no PASS band) |
| serial-read origin | per-fan-out read origin | not logged by any row family | **UNMEASURED** |
| default ON | all of the above pass | readiness fails, parity unmeasured | **keep OFF** |

## Protocol and capture identity

| fact | value |
| --- | --- |
| launch | `./run_bot.sh start` (detached, `setsid`; no `--live`, so the driver's `dry_run` is on) |
| bot start | 2026-11-10T08:09:29-08:00 (pidfile 336754 = `uv run` wrapper; driver pid 336778) |
| warm-up end | 2026-11-10T08:12:13.949-08:00, the first observed successful delta fan-out (`fanout=4`, `current_block=26385627`, `anchor=26385626`) |
| clean stop | `./run_bot.sh stop` returned at 2026-11-10T09:12:36-08:00; last log row `[runner] bot exited rc=143 2026-11-10T09:12:35-08:00` |
| soak duration after warm-up | 3,621 s = 60.35 min (warm-up to the `[runner] bot exited` row; `run_bot.sh stop` returned at 09:12:36) |
| commit | `3f00fac2c11ac947ba95dc2a8f4916e9690c36d9` (`docs(simulation): evaluate BAL carry-forward dispatch`); worktree clean at start |
| extension build | `src/degenbot/_ffi.abi3.so`, 324,166,001 bytes, mtime 2026-11-10 03:09, newer than every Rust source |
| owner / executor | `EXECUTOR_OWNER_ADDRESS=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266`, `DEGENBOT_INJECT_EXECUTOR_CODE=1`, `EXECUTOR_RUNTIME=/tmp/dryrun_executor_runtime.txt` (sha256 `1365bc2102a1b29d8028b845d7a67e4fddaff585c4d59fbf25937154c3ea7339`, 37,499 bytes on disk, decoded 18,748 bytes as loaded) |
| flag / logging | `DEGENBOT_DELTA_DISPATCH=1 DEGENBOT_DEBUG=1 RUST_LOG=degenbot=debug,degenbot::sim=trace` |
| node | `reth/v2.10.1-58ebce1/x86_64-unknown-linux-gnu`, chain id `0x1`, `http://host.containers.internal:8545` / `ws://host.containers.internal:8546` |
| resolved RPC (bot row) | `http=ws://host.containers.internal:8546 ws=ws://host.containers.internal:8546` |
| backfill | `{seed=26379518, ws_block=26385615}` (6,097 blocks) |
| connector index | `{edges=540366}` |
| executor image loaded | `{bytes=18748, path=/tmp/dryrun_executor_runtime.txt}` |
| block range | 26385619..26385928 (capture); 26385627..26385928 post-warm-up |
| disk at start | 536 G available of 1.9 T |
| final process state | bot not running; sampler exited (exit 0); no soak process remains |

## Raw evidence paths

| artifact | path |
| --- | --- |
| preserved console capture | `logs/bal_production_soak_on.log` (932,451,415 bytes, 4,162,435 lines) |
| `fanout-trace` extract | `logs/bal_production_soak_trace.log` (2,647 rows) |
| 15-second RSS samples | `logs/bal_production_soak_rss.csv` |
| wall stamps for timestamp-less rows | `logs/bal_production_soak_events.jsonl` |
| end-of-run metric scrape, taken ~09:12:24 while the bot was still live | `logs/bal_production_soak_metrics_end.txt` |
| machine-readable analysis | `logs/bal_production_soak_analysis.json` |
| sampler source | `logs/bal_production_soak_monitor.py` (also `/tmp/bal_production_soak_monitor.py`) |
| analyzer source | `logs/bal_production_soak_analysis.py` (also `/tmp/bal_production_soak_analysis.py`) |
| pre-flight identity record | `/tmp/bal_production_soak_preflight.txt` |

`logs/` is gitignored, so these are local evidence, not tracked artifacts.

## Capture integrity

| check | result |
| --- | --- |
| trace extract rows / mismatches | 2,647 / 0 (`rg --no-filename 'fanout-trace '`) |
| `bal-head` rows | 312 total; 302 post-warm-up, all `settled`/`carried` |
| fenced fan-out selection rows | 26 fan-out ids existed (1..26); 23 post-warm-up, all 23 with a surviving selection row |
| fan-out ids without a selection row | 3 (`fanout=1,2,3`), all pre-warm-up, lost in the boot/backfill burst |
| `cands=` rows | 26 (one per fan-out id) |
| `storage prefetch` rows | 3, each on a `legacy` fallback |
| retired-memo rows | 255 |
| cordon rows | 9 (5 exits; `entered` counter reached 5) |
| process restarts | 0 (single driver pid 336778 for the whole capture; `pid_changes=0`) |
| reorgs | none in the log (0 `reorg` rows); `degenbot_reorg_recovery_dropped_total` reads 11 at the end-of-run scrape, which is the only scrape preserved |
| RPC errors | none logged; the scrape exposes no RPC-error counter family |
| ERROR rows / WARN rows | 0 / 23 (cordons, held-window notices, GIL warnings) |
| console records dropped | `degenbot_log_dropped_total_total{sink="console"}` 1,449,712 at the first post-warm-up scrape (~08:12:50, recorded in the pre-flight file; that scrape was not preserved) -> 1,564,399 at the end-of-run scrape (+114,687 during the soak that followed it) |

The drop count matters for what the capture can prove. The console sink is a
bounded drop-counting queue, so every row count is a lower bound. In this run
the boot/backfill burst dropped all three pre-warm-up selection rows and 25 of
the 80 post-warm-up candidate `fanout-trace` rows (55 attributed candidates
survive against 80 candidates the `cands=` rows report). Every fan-out id that
survives in either family kept both its selection row and its `cands=` row — the
ids run 1..26 with no gap — so the readiness census below covers every
post-warm-up fan-out the capture can see. A fan-out that lost both rows would be
invisible, and the candidate-level counts are lower bounds.

## Readiness

Excluding warm-up, one selection row per fenced fan-out:

| lane | fan-outs | share |
| --- | --- | --- |
| `delta` | 20 | 86.96% |
| `legacy` | 3 | 13.04% |
| total | 23 | 100% |

The band requires 90.91%, so the measured share **fails**. The denominator is
23 fenced fan-outs, and each `legacy` event costs about 4.3 points, so the band
is sensitive to a single event at this sample size.

### Anchor class separates the two populations

| fan-out anchor | fan-outs | lanes | cold reason |
| --- | --- | --- | --- |
| anchor at a previous head (`anchor < current_block`) | 20 | `delta` 20 | none |
| anchor at the block being asked (`anchor == current_block`) | 3 | `legacy` 3 | `deadline` 3 |

All three fallbacks are same-block asks: a candidate solved at the head that
had just arrived, the fan-out asked that head's own anchor immediately, and the
head's BAL prepare had not finished, so the dispatch took the shipped legacy
lane. The three heads' own prepare walls were 282,076 us, 211,764 us, and
69,553 us — all above the 50 ms readiness deadline
(`DEVELOPMENT_READINESS_DEADLINE`). The other 20 fan-outs anchored at a prior head
and all resolved `delta`.

### Cold-reason census

| population | label | count | per 1,000 |
| --- | --- | --- | --- |
| settled heads (n=302) | no cold reason (`refusal=null`, `bal=null`) | 302 | 0.0 cold |
| settled heads (n=302) | `lineage-refused`, `bal-unreadable`, `not-consecutive`, any BAL skip | 0 | 0.0 |
| fenced fan-outs (n=23) | `delta`, reason `None` | 20 | 869.6 |
| fenced fan-outs (n=23) | `legacy`, reason `deadline` | 3 | 130.4 |
| fenced fan-outs (n=23) | `undelivered` / `unprepared` / `lineage-refused` / `bal-unreadable` | 0 | 0.0 |

No head transition was cold: every post-warm-up head was `settled`/`carried`,
so every head's BAL fetch and decode succeeded and zero heads were refused.

### Settled heads serving their own anchor

22 of the 302 settled heads had a fenced fan-out anchored at their own number.
19 were served `delta` (20 delta fan-outs over those 19 heads); 3 were
served `legacy` with reason `deadline`:
anchors 26385837, 26385839, and 26385881. The band requires zero violations, so
this half of the readiness criterion also **fails**. All 280 settled heads whose
own anchor was never exercised are unmeasured for this criterion, not passing.

**Reading.** The failure is not a lane-wide readiness collapse. It is one
structural class — same-block-anchored fan-outs, which ask a head that is still
preparing. Every prior-block ask (20 of 20) got `delta`; the three same-block
asks are the only `legacy` events. The prepare wall is two orders of magnitude
below the head interval (next section) but 1.4 to 5.6 times the 50 ms deadline
these asks race against, and 240 of the 302 post-warm-up heads took more than
50 ms to prepare, so most heads cannot serve a same-block ask inside that
deadline. Each such failure breaches the zero-violation half of the band
outright; the share half tolerates two, so the third is what tips it under
(21 of 23 = 91.3% would clear the 90.91% bar).

## Prepare wall

Raw `prepare_us` from the 302 post-warm-up `bal-head` rows, and head intervals
from the wall stamps of consecutive observed head rows:

| wall | n | p50 | p90 | p95 | max | mean |
| --- | --- | --- | --- | --- | --- | --- |
| prepare (ms) | 302 | 80.68 | 202.87 | 253.20 | 451.97 | 104.85 |
| head interval (s) | 301 | 12.000 | 14.000 | 14.002 | 15.997 | 11.994 |

| ratio | value | band |
| --- | --- | --- |
| p95 prepare / median head interval | 2.11% | <= 25% PASS |
| max prepare / median head interval | 3.77% | < 1 interval PASS |

The observed interval distribution is 8.0–16.0 s with a 12.0 s median, so the
p95 wall fits 47 times inside a median block and the worst single wall fits 26.5
times inside it. The band passes on both halves.

## Memory

RSS is sampled every 15 s from the actual driver process (`VmRSS` of
`/home/dev/.venvs/degenbot/bin/python examples/eth_settlement_arbitrage_v2_v3_v4_rust.py`,
pid 336778): 229 samples with a live process, 226 of them post-warm-up.

| fact | value |
| --- | --- |
| first post-warm-up 5-min median | 4,384,648 KiB (4.18 GiB), 19 samples |
| last 5-min median (before stop) | 4,462,040 KiB (4.26 GiB), 19 samples |
| growth | +1.77% (band <=15%) PASS |
| linear slope over the window | +1.24 MiB/min |
| min / max post-warm-up | 3,711,596 / 4,601,504 KiB (max at 08:30:31) |
| baseline at warm-up | 3,711,596 KiB |
| process restarts | 0 |

The band passes, and the slope is reported rather than hidden: growth is
positive at +1.24 MiB/min, so this hour establishes "under 15% over 60 minutes",
not a proven plateau. The min/max spread (3.54–4.39 GiB) includes the boot-state
ramp, which is why the first 5-minute median is already above the warm-up
baseline.

### Generation identities

| identity | first | p50 | p90 | max | last | max at |
| --- | --- | --- | --- | --- | --- | --- |
| `held` | 3,877 | 3,658 | 4,444 | 5,567 | 3,908 | anchor 26385634 |
| `live` | 3,481 | 2,493 | 2,806 | 3,710 | 2,408 | — |
| `expired` | 396 | 1,120 | 1,878 | 3,226 | 1,500 | — |

`p50` is nearest-rank over the 302 raw rows, the method the prepare table uses;
the analysis JSON's identity `p50` fields use the sample median, which lands at
3,659 (`held`), 2,493 (`live`), and 1,123 (`expired`).

`expired` was non-zero on all 302 rows. `held` does not grow monotonically:
37 fold drops occurred (largest single drop 2,948 identities), the first-half
median is 3,670 against a 3,635 second-half median and a 3,583 last-20-minute
median, and the fitted slope is -1.95 identities per minute. The largest `held`
(5,567) is an early-window peak, and the unbounded-chain reading from the
pre-retention revision (6,420 carried at its 27th head) does not reproduce here
across 302 heads.

**Reading.** The retention policy bounds an entry's idle lifetime, not the
store's size; this hour shows a bounded, fold-oscillating identity count and a
1.77% RSS band, not an absolute cap. Identity counts are entries, never bytes.

## Verdict and failure parity — UNMEASURED

This capture has no OFF arm, no matched head set, and no replayed candidate set,
so it cannot compare verdicts, failure buckets, or per-candidate read sets
between lanes. **PARITY UNMEASURED.** Nothing in the capture signals a storage
divergence — no desync, no quarantine, no verify-mismatch, no reorg row — and
the armed engine-vs-RPC divergence probe logged 26 per-fan-out summaries over
91,724 compared slots with zero divergent slots, zero divergent pairs, zero
divergent pools, and no per-mismatch `[sim-divergence] pool=` line. That probe
compares the engine's packed state against RPC-served values on fallback reads
during armed re-simulations; it does not compare the two lanes, so a clean probe
tally is supporting evidence, not a parity measurement.

Descriptive comparison against the prior OFF window
(`logs/bal_dispatch_v2_off.log`, a different, earlier window on a different
build era, with zero shared heads):

| fact | prior OFF | this ON |
| --- | --- | --- |
| trace rows | 399 | 2,647 (2,447 post-warm-up) |
| trace verdicts | `none` 399 | `none` 2,377, `result` 70 post-warm-up |
| fenced fan-out lanes | `legacy` 9 | `delta` 20, `legacy` 3 post-warm-up |
| `bal-head` rows | 0 (predates the row) | 312 |
| drained batches / candidates | 515 / 603 | 2,641 / 2,803 |
| ok / failed / exceptions | 0 / 603 / 0 | 0 / 2,803 / 0 |
| failure buckets | `CurrencyNotSettled` 302, `ERC20: transfer amount exceeds balance` 183, `UniswapV2: TRANSFER_FAILED` 43, `TF` 38, `PoolNotInitialized` 27, `UniswapV2: INSUFFICIENT_LIQUIDITY` 10 | `CurrencyNotSettled` 1,407, `ERC20: transfer amount exceeds balance` 986, `UniswapV2: TRANSFER_FAILED` 170, `TF` 160, `UniswapV2: INSUFFICIENT_LIQUIDITY` 68, `PoolNotInitialized` 6, `empty` 6 |

The two windows simulated different candidate sets at different heads, the
bucket mixes differ, and the ON window's 70 `result` verdicts are all
unattributed inline-hook re-quote rows, not fenced fan-outs. This table is a
description of two windows, never a lane comparison.

## Fallback RPC cost

| term | value |
| --- | --- |
| legacy fan-outs | 3 (all same-block asks, `deadline`) |
| legacy prefetch batches | 3, one per fallback |
| prefetch members requested / seeded | 271 / 271 (87, 106, 78) |
| BAL requests per accepted head | 1 by construction; 302 observed head rows = 302 requests |
| header reads per fan-out | not separately logged — UNMEASURED |
| serial read per fallback | not separately logged — UNMEASURED |

Cold-storage aggregates over the 23 fenced fan-outs (`cands=` counters):
`b_fast=0`, `b_slow=51`, `b_ms=20`, `code=0`, `c_ms=0`, `s_fast=1507`,
`s_slow=650`, `s_ms=4031`, `bh=0`, `warm_basic_hits=438`, `slow_ms=4056`.
The counters aggregate every cold read a fan-out paid, so a fallback's own
serial read is not separable from them.

**Reading.** The fallback rate (13% of fenced fan-outs) is low, and its measured
cost is three prefetch batches of 271 members total plus whatever the serial
read costs — a cost the capture cannot price. A 13% fallback rate on this
denominator does not by itself erase the delta lane's saving, but the serial-read
term is exactly the part this evidence lacks.

## Candidate density, fan-out durations, verdicts

| fact | value |
| --- | --- |
| candidate-bearing blocks post-warm-up | 226 of 302 |
| `fanout-trace` rows post-warm-up | 2,447 (8.10 per head) |
| attributed to a fenced fan-out / unattributed | 55 / 2,392 |
| candidates per fenced fan-out (`cands=`) | 80 total, mean 3.48, max 36 |
| delta fan-out wall (`fanout_ms`, n=20) | p50 11, p90 62, p95 533, max 617, mean 68.5 |
| legacy fan-out wall (`fanout_ms`, n=3) | p50 14, max 23 |
| trace verdicts post-warm-up | `none` 2,377, `result` 70 (all unattributed) |
| drained batches / candidates (whole capture) | 2,641 / 2,803 |
| ok / failed / exceptions (whole capture) | 0 / 2,803 / 0 |
| retired-memo rows (post-warm-up) | 252; hits 385,892, misses 122,762 |

`fanout_ms` is a millisecond-resolution dispatch wall; the delta p95/max are
driven by two fan-outs (617 ms with 2 candidates, 533 ms with 3), so the lane's
tail is not a distribution over many wide fan-outs.

## Confounders and limits

1. **Boot-burst console drops bias row counts, not post-warm-up fan-out lanes.**
   1.45 M records were already dropped at the first post-warm-up scrape
   (~08:12:50); the remaining 114,687 of the 1.56 M total accrued over the soak
   that followed it. All three pre-warm-up selection rows were lost; no
   post-warm-up fenced fan-out row was lost. Candidate-level trace rows remain a
   lower bound (55 of 80).
2. **Fenced fan-out density is clumpy and market-driven.** Two fan-outs in the
   first 40 minutes, then six within blocks 26385834–26385842, then sparse. The
   readiness denominator is 23, so one event moves the band by ~4.3 points.
3. **The fleet cordoned five times** (`EventBurst` intake holds, 9 cordon rows,
   the last near the end of the window) and the registration crawl recorded
   380,934 `pool-build-error` skips. Both are background load that can shape
   candidate supply; neither is a dispatch-path event.
4. **The prior OFF arm is a different window** (2026-11-10 ~01:12 vs 08:09), a
   different build era, and shares no head with this run.
5. **The executor runtime path is the established dry-run identity**, and its
   decoded length matches the image the earlier live captures recorded
   (18,748 bytes). Its sha256 does not match the repository's
   `contracts/cmd_executor_runtime_bytecode.txt`; the file is recorded here, not
   resolved, and no production code was changed to reconcile them.
6. **One hour is one hour.** The memory band is a 60-minute first-versus-last
   comparison with a positive slope; it does not prove a multi-hour plateau.
7. **Head-interval resolution comes from the sampler's row wall stamps**, not
   the 15 s RSS cadence, so the interval table is independent of that cadence —
   but the stamps carry the sampler's 2 s log-scan granularity, which bounds the
   interval resolution.

## Reproduction

Launch (exactly the protocol environment; detached):

```bash
cd /workspaces/degenbot
EXECUTOR_OWNER_ADDRESS=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266 \
DEGENBOT_INJECT_EXECUTOR_CODE=1 \
EXECUTOR_RUNTIME=/tmp/dryrun_executor_runtime.txt \
DEGENBOT_DELTA_DISPATCH=1 DEGENBOT_DEBUG=1 \
RUST_LOG='degenbot=debug,degenbot::sim=trace' \
./run_bot.sh start
```

Sampler (15 s RSS; wall stamps for the timestamp-less row families). Start it
with the launch — it truncates and recreates `logs/bal_production_soak_rss.csv`
and `logs/bal_production_soak_events.jsonl` — and stop it after
`./run_bot.sh stop`. In this run its first sample is
2026-11-10T08:11:35-08:00, about two minutes after the launch:

```bash
python3 /tmp/bal_production_soak_monitor.py
```

Scrape metrics **before** stopping the bot. The endpoint is an in-process thread
of the bot (`degenbot-metrics`; `DEGENBOT_METRICS_ADDR`, default
`127.0.0.1:9464`), so it is gone as soon as `./run_bot.sh stop` returns:

```bash
cd /workspaces/degenbot
curl -s http://127.0.0.1:9464/metrics > logs/bal_production_soak_metrics_end.txt
```

This capture's scrape lands in the last live minute: its
`degenbot_process_rss_bytes` of 4,585,086,976 B (4,477,624 KiB) is the RSS of
the 09:12:24 sample, and the `[runner] bot exited` row is stamped 09:12:35.

Then stop the bot, and preserve and extract afterwards — the console capture is
the file the stopped run leaves behind:

```bash
cd /workspaces/degenbot
./run_bot.sh stop
cp logs/bot_run.log logs/bal_production_soak_on.log
rg --no-filename 'fanout-trace ' logs/bal_production_soak_on.log \
  > logs/bal_production_soak_trace.log
```

Analyze (single ON arm; the prior OFF capture is read descriptively only):

```bash
python3 /tmp/bal_production_soak_analysis.py \
  --on logs/bal_production_soak_on.log \
  --trace logs/bal_production_soak_trace.log \
  --rss logs/bal_production_soak_rss.csv \
  --events logs/bal_production_soak_events.jsonl \
  --off logs/bal_dispatch_v2_off.log \
  --out logs/bal_production_soak_analysis.json
```

The analyzer reuses `scripts/bal_live_ab_analysis.py`'s row parsers, so a row
family added or retired there moves this report with it.

## Unmet acceptance

- Readiness delta share: **FAIL** at 86.96% against the 90.91% band; the
  same-block anchor class is 0 of 3 delta.
- No settled head fails its own anchor: **FAIL**; three settled heads
  (26385837, 26385839, 26385881) served `legacy` to a fan-out anchored at their
  own number.
- Verdict/failure parity: **UNMEASURED** — no matched OFF arm or replayed
  candidate set in this capture.
- Serial-read origin in the fallback cost: **UNMEASURED** — no row records it.
- Per-fan-out header-read overhead: **UNMEASURED** — no row records it.
- 280 of 302 settled heads were never asked at their own anchor, so that half of
  the readiness band is exercised on 22 heads only.
- Default ON: **not supported**; two criteria fail or are unmeasured, so the
  promotion gate is unmet and the flag stays default OFF.
