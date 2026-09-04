---
name: bot-telemetry
description: Work with degenbot's telemetry — retrieve and interpret Jaeger traces and Prometheus metrics, diagnose bot performance from them, or add new spans/metrics/instruments. Use when investigating slow traces, sparse or missing Jaeger spans, pump/solve/registration latency, Grafana dashboards, or wiring new instrumentation.
---

# Bot Telemetry

How to see what the live bot is doing, attribute time to code, and extend the instrumentation when the existing spans cannot answer the question.

The pipeline: Rust-core spans flow through one tracing registry in `degenbot-python/src/python_log_layer.rs` → OTLP exporter → **Jaeger** (service name `degenbot-bot`). The same init starts a **Prometheus scrape endpoint** exposing the instruments defined in `degenbot-bot/src/instruments.rs` + `metrics.rs`. Log-level control (RUST_LOG etc.) is a separate surface documented in [`docs/logging.md`](../../../docs/logging.md) — read it when the question is about log lines, not spans.

## Step 1: Confirm the pipeline is live

Before interpreting anything, prove data is flowing — a silent exporter wastes every step after it.

```bash
# Spans reaching Jaeger (any recent trace counts)
curl -s 'http://host.docker.internal:16686/api/services'
# Metrics endpoint answering (default 127.0.0.1:9464; override DEGENBOT_METRICS_ADDR)
curl -s http://127.0.0.1:9464/metrics | head
```

`jq` is not installed; parse JSON with a `/tmp/*.py` script or `python3 -m json.tool`.

Done when you hold one fresh trace ID and one non-empty metrics sample. If empty:

| Symptom | Check |
|---|---|
| No service in Jaeger | Dev builds compile the OTel layer by default; release wheels ship without it (`otel` is dev-only via `[tool.maturin] features`). `DEGENBOT_OTEL=0` opts out at runtime — verify it is unset. Endpoint precedence: `OTEL_EXPORTER_OTLP_ENDPOINT` env > `otel.endpoint` in `~/.config/degenbot/config.toml` > `http://localhost:4318`. From inside the devcontainer use `host.docker.internal`, not localhost. |
| Empty metrics response | The scrape endpoint only starts when the OTel layer is active, same gate as above. Address precedence: `DEGENBOT_METRICS_ADDR` env > `otel.metrics_addr` config > `127.0.0.1:9464`. Host-side Prometheus must also be scraping that address before Grafana shows anything. |
| Service exists but few spans | Sparse capture was the historical baseline — most spans were added deliberately. Check which sites exist (Step 3) before concluding data is lost. |

## Step 2: Pull and rank what the symptom lives in

Query the Jaeger API for the span names of interest, then rank by duration:

```bash
curl -s 'http://host.docker.internal:16686/api/traces?service=degenbot-bot&operation=degenbot.arb.solve&limit=20&lookback=1h'
```

Core span names:

- `degenbot.pump.block` — per-block drain (field `block.number`); parent of the block's work.
- `degenbot.arb.solve` — solve dispatch per block (field `block.number`).
- `degenbot.path.register` — path registration (field `hops.count`).

Metric families (meter `degenbot-bot`, all prefixed `degenbot.`): latency histograms `block.header_to_solved`, `solve.duration`, `simulate.duration`, `drain.queue_wait`, `log.decode`, `state.apply`; gauges `drain.queue_depth`, `state.head_lag_blocks`, `pump.seconds_since_header`, `pump.seconds_since_apply`, `engine.registered_paths`; counters `logs.received/applied/apply_missed`, `candidates.found`, `solver.clamps`, `submit.outcomes`, `errors`. Scrape `curl http://127.0.0.1:9464/metrics` for the live list — the instruments source is authoritative when this list drifts.

Grafana dashboards + alert rules live in `docs/grafana/` (`degenbot-overview.json`, `degenbot-alerts.yml`, `ALERTS.md`).

Done when you can state, with numbers, which phase owns the time: drain/decode, state apply, solve, simulate, or submit. A histogram p99 plus a ranked trace sample is enough evidence to proceed.

For function-level attribution below the span granularity, use hotpath profiling instead of adding ad-hoc timers — the recipe (env vars, report formats, TUI console) is in `AGENTS.md` under **Profiling**.

## Step 3: Drill into the owning phase

Read the span site behind the hot phase to see its fields and children:

- `bot_core/block_pump.rs` (~line 1201) — pump block span
- `solvers/arb_engine/engine_handle.rs` (~line 129) — solve span
- `solvers/arb_engine/lifecycle.rs` (~line 97) — registration span

If the span's fields answer the question, interpret and move on. If not, add instrumentation (Step 4) rather than inferring from indirect evidence.

Two traps while interpreting:

- **Tripwire / invariant firings are always real signals.** Since ADR-040 the reaction is per-bucket: tainted classes (`MissedLog`, `UnhandledReorg`, `StorageMutated`) **quarantine the divergent pool** (recorded via `degenbot.quarantine.events`) and the session keeps running; fatal classes (`ws_completeness`, drain watchdogs) still exit loudly. Treat any firing or quarantine as urgent root-cause work with a repro dump under `logs/desync/` — never as noise to suppress. The pool-id namespace fix (V4 ids offset by `1<<32`) closed the historical cause.
- **Correlate across sinks by timestamp.** Jaeger traces, Prometheus samples, and `/tmp/bot_run*.log` lines each carry partial context; line up timestamps before drawing a conclusion from any single sink.

## Step 4: Add telemetry following the house pattern

Match the grain of the question to the instrument:

- **Phase timing around existing functions** → `#[hotpath::measure]` / `hotpath::measure_block!` (no-op unless built with the feature + runtime gate). Pattern and extension rules for widening to other crates: `AGENTS.md` **Profiling** section.
- **Distributed timing visible in Jaeger** → a `tracing::info_span!` at an existing call site like the ones in Step 3, carrying structured fields (`block.number = …`, `hops.count = …`).
- **Aggregate behaviour over many blocks** → a metric in `instruments.rs`: declare on `Instruments::new` (histogram with `LATENCY_BUCKETS_SECONDS`, gauge, or counter), record from the owner of the phase, and mirror the declaration shape of its neighbours.

Same discipline as `log::debug!`: sprinkle liberally — the cost model already handles no-op builds.

Done when a probe run shows the new signal end-to-end (span in Jaeger or sample in `/metrics`), not merely compiling.

## Step 5: Close the loop with before/after numbers

A performance change is done when the telemetry that motivated it shows the delta: re-pull the ranking from Step 2 against the new build and state both numbers. Prefer improvements whose effect is visible in the shared dashboards over one-off scripts, so the win stays observable after the session ends.
