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
