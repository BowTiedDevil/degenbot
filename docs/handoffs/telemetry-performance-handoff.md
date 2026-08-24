# Handoff: telemetry-driven performance work — degenbot

**Date:** 2026-08-24 · **Author:** pi research session · **For:** the agent owning OTel/metrics-driven performance of the live degenbot arbitration bot.

**Status:** Pool-id collision bug root-caused, fixed, and verified on the live graph; workspace build fixed. Two commits landed. The live bot is running with full OTel tracing + Prometheus metrics + Grafana. Ready to start telemetry-driven performance investigation.

## Where we are

We've been working on the degenbot arbitrage bot in `/workspaces/degenbot` (Rust core crates in `rust/crates`, Python in `src/degenbot`, both built via `uv`). We have full OTel tracing, Prometheus metrics, and Grafana wired up, and the bot is running live. Recent focus was expanding telemetry so we can get real insight into the live bot.

**Two commits landed since the last deliberate handoff:**

- `770ca0f4b` — **fix(pathfinding): namespace V4 pool ids so collided V2/V3 rows can't alias**. Root-caused the pool-id collision bug: the V4 `managed_pools.id` counter is independent of the V2/V3 `pools.id` counter, and 116,224 of 125,003 live V4 ids collide. Before the fix the DFS treated two distinct pools as one (ids overlapped in the edge list), yielding paths that could not close in token space — surfaced live as 85k `direction-fail` skips / 0 registrations, measured 148,896 broken paths of 10.6M. Fix: V4 graph ids are now namespaced `managed_pool_id + 1<<32` (`V4_POOL_ID_OFFSET`, mirrored in Python `_V4_POOL_ID_OFFSET`), demangled in `_build_path_steps`. Verified on the live graph: **10,645,844 paths yielded, 0 broken**. degenbot-db 150 tests green (incl. 7 pathfinding-parity, fixture regenerated), Python 14 tests green.
- `76728244a` — **fix(build): gate MiddleRegistryOtel behind otel feature** (pre-existing break where `cargo build --workspace` without `--features otel` failed).

## Environment / how to interact with the live system

- We are inside the devcontainer. Promote/run the Rust bot via `uv run` (the bot is live now, from `~/.config/degenbot`).
- **Jaeger** UI at `http://host.docker.internal:16686` (service `degenbot-bot`). Query API: `curl -s 'http://host.docker.internal:16686/api/traces?service=degenbot-bot'`. `jq` is NOT installed — pipe through `python3 -m json.tool` or a small py script.
- **Metrics** on the host Prometheus (Grafana + Prometheus on the dev host, reachable from the devcontainer). Prometheus scrape target scrapes the bot's metrics port from inside the container. Grafana dashboards imported (mostly from JSON added under `docs/grafana/`); they were sparsely populated until we wired the Prometheus scrape + exposed the port. Verify via `curl` to the metrics endpoint first.
- Bot config: `~/.config/degenbot/config.toml` (`otel.enabled` + `otel.metrics_addr` keys). Env gate `DEGENBOT_OTEL=1` turns tracing on. Bot logs land under `/tmp/bot_run*.log` (old runs: `bot_run12/13/14.log`).
- There's a 50k `MAX_REGISTERED_PATHS` path cap (`DEGENBOT_MAX_PATHS` env override) so the bot reaches steady state and per-path solve is observable without registration load.
- The **direction-fail fail-stop** is wired into `direction` resolution — it aborts registration loudly on a token-mismatch invariant breach. Don't trust the bot silently surviving after this; check logs + Jaeger. With the pool-id fix landed, that class of failure should be gone — but treat any firing as urgent (real invariant breach, don't paper over).

## What to do this session — telemetry-driven performance investigation

1. **Verify the live bot is healthy and tracing is capturing far more than the sparse baseline.** Check Jaeger for `degenbot-bot` spans across a lookback (confirm per-block pump spans, solve spans, registration spans are showing). If spans are still sparse, dig into `rust/crates/degenbot-bot/src/otel.rs`, `instruments.rs`, `metrics.rs`, and the span sites in `bot_core/block_pump.rs`, `solvers/arb_engine/engine_handle.rs`, `solver_dispatch.rs` — earlier work added OTel plumbing there.

2. **Find slow traces.** Pull Jaeger traces and rank by `duration`. Drill into the slowest ones — is the time in pathfinding (DFS discover), pool building, solve (`degenbot_solvers` / `mobius_v3_int`), simulation (`degenbot-bot/src/solvers/arb_engine`), or submission (`degenbot-submission/src/submit.rs`)? Identify whether spans have the attribution you need, or whether you must add child spans / `#[instrument]` + key fields (pool count, depth, block number) to make root-causing possible.

3. **Correlate with metrics.** Prometheus/Grafana should show pump latency, solve time, registration throughput. Known hot-spots from prior sessions:
   - The **path-cap crawl** previously burned ~265% CPU counting skips; a `DiscoveryCrawlComplete` sentinel (in `src/degenbot/runner/build_paths.py`) now stops the crawl. Confirm it's not regressed.
   - **Lazy snapshot** behavior and the **rayon-in-solve** question (`degenbot-pathfinding` DFS + solver rayon parallelism — "Check rayon usage in solve" was an open thread).
   - Registration pipeline: `src/degenbot/arbitrage/engine_registry.py`, `rust/crates/degenbot-bot/src/bot_core/liquidity_verifier.rs`.

4. **Make performance improvements using the telemetry** (the point of expanding OTel/metrics in prior sessions — real insight to drive concrete wins). Prefer improvements validated by before/after numbers pulled from Jaeger/Prometheus. Commit each completed task separately.

## Guardrails / gotchas

- **Commit after each completed task**; if you can't, leave a clear `TODO` + a `/tmp` notes file.
- Sandbox tooling in this repo is flaky in specific ways:
  - `rg` regex-parse errors — use `rg -n 'literal'` (avoid complex alternations with unclosed groups).
  - Inline multi-line strings through fabric can hit TS type errors — write `/tmp/*.py` scripts and run via `uv run python`, then `tail`.
  - `jq` is not installed — use `python3 -m json.tool` or a py script.
  - `cast` on host RPC can 403 — wrap/retry or fall back to the DB.
- The direction-fail fail-stop is loud on purpose — if it fires, treat as urgent, don't paper over.
- If in doubt about semantics, prefer reasoning from committed code over assuming.
- User is available but was AFK; operate autonomously, keep them posted with concrete numbers.

## Tool surface used this session

`fabric_exec` with `code` + `strings` params; `pi.bash`/`cd`, `pi.write` (temp scripts), `pi.read`. All bots/tools route through these.

## Handoff status — 2026-08-24 session (telemetry scrape + critical fix)

### Work done this session
- **chore(bot) `6f2bacd75`** — run_bot.sh now defaults `DEGENBOT_OTEL=1` so EVERY run exports OTel spans (Jaeger) + Prometheus metrics (operator can force off with `DEGENBOT_OTEL=0`).
- **fix(pathfinding) `a5335943a` — CRITICAL, unblocked the live bot.** The Rust `build_path_graph`/`fetch_path_graph_edges` seam keys `v4_lookups`, `pool_id_to_kind(+_string)` and `edges` by NAMESPACED V4 ids (`managed_pool_id + 1<<32`, introduced in `770ca0f4b`), but `_build_path_steps` still demangled to the RAW id before the `v4_lookups` lookup. On any colliding V4 pool this raised `KeyError` (live: 9378) and crashed the bot on the first V4 path during registration — it never reached the 5-min soak. Fix: look up `v4_lookups[pool_id]` with the namespaced id directly (consistent with `pool_id_to_type`). The collision unit test had faked RAW-keyed `v4_lookups` (matching the old demangle), which is why it slipped through 14 green tests; updated it to the real NAMESPACED-key contract. Verified: 2506 Python tests green, degenbot-db 150 Rust tests green (incl. 7 pathfinding-parity), live-DB repro renders V4 PathSteps with correct manager/hash.

### Bot run for the 5-min soak (telemetry)
- Start: `DEGENBOT_SIM_EXIT_ON_FAIL=0 ./run_bot.sh start` (the default `DEGENBOT_SIM_EXIT_ON_FAIL=1` fail-stop exits on the FIRST sim revert — routine for a thin-margin soak, so it must be overridden to stay alive through a multi-minute telemetry window).
- Healthy at 5-min mark (42k+ registered paths of the 50k `DEGENBOT_MAX_PATHS` cap), 355k+ log lines, 0.0s drain queue (worker not starved).

### Scraped telemetry (Jaeger 5-min window + Prometheus metrics)
Span coverage is far beyond the sparse baseline: `degenbot.pump.block`, `degenbot.arb.solve`, `solve_all_paths`, `degenbot.simulate.dispatch`, `degenbot.bundle.simulate`, `degenbot.bundle.dispatch`, `dispatch`, `degenbot.path.register`, `degenbot.pool.register`, `degenbot.pool.verify_lifecycle`, `degenbot.jaeger.e2e` all present (13 ops).

**CTQ numbers (Prometheus, ~6 min uptime):**
- solve: 26 solves, sum 17.44 s -> avg 671 ms/solve (block 25826284: 685 ms; 25826285: 525 ms).
- simulate: 1172 sims, sum 5.29 s -> avg 4.5 ms/sim, ~45 sims per solve (fan-out).
- block_header_to_solved: 54, avg 618 ms.
- engine_registered_paths: 44,659; candidates_found 3,336; state_apply avg ~0.5 ms; log_decode ~0.6 us; drain_queue_wait 0.46 s / 71 samples.

**Structural hot-path findings (drill-in):**
1. **Solve is the dominant per-block cost** — `degenbot.arb.solve` was 62% of a `degenbot.pump.block` (685 ms of 1.11 s). All solve/block_header_to_solved buckets fall in `le=5` (buckets defined in seconds; these ops are ms — coarser than ideal for latency histograms). Per-solve work is the top target.
2. **Simulate-dispatch overhead dwarfs the actual sims** — trace `abc162378fa1`: `degenbot.simulate.dispatch` 411 ms containing `degenbot.bundle.simulate` children summing ~28 ms (16/4.9/3.5/3.1 ms). ~383 ms of that dispatch is NOT in bundle.simulate — it is fan-out coordination, candidate generation, and the `Python::attach`/GIL-result-set handoffs on the pyo3-async dispatch path (`crates/degenbot-python/src/simulation/dispatch.rs`). This is the `--node-ws`/python-async dispatch seam flagged as an open question; the dispatch span currently has only `current_block` + `phase_candidate_count` attrs — it needs child spans (candidate build, GIL handoff, join) or the phasic `measure_block!` event markers promoted to child spans to attribute the 383 ms.

### Recommended next steps (not yet done — stopped at scrape + structural ID)
- Add child spans / `#[instrument]` to `degenbot.simulate.dispatch` (candidate-gen, sim fan-out, GIL `Python::attach`, join) and to `degenbot.arb.solve` (DFS discover vs pool build vs mobius solve) so the next scrape can attribute the 671 ms solve and 383 ms dispatch overhead precisely. Re-scrape for a before/after.
- Confirm whether the solve path is serialized under the GIL vs rayon — the pooled-path solve fan-out (`solve_all_paths`) and simulation dispatch both GIL-handoff; check parallelism before optimizing single-threaded.
