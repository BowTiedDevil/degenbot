# hotpath profiling on Prometheus/Grafana (epic 5GXLG5)

[hotpath-rs](https://hotpath.rs) profiles the block-pump -> solve path (the
`#[hotpath::measure]` sites across `block_pump`, `SolveCoordinator`,
`EngineHandle`, and the `cl_solve.*` / `mixed.*` solver internals) and, with
the `hotpath-prometheus` build feature, exposes every profiling subsystem as
Prometheus metrics on a dedicated `GET /metrics` endpoint. The companion
dashboard is [hotpath-profiling.json](hotpath-profiling.json) — imported into
Grafana and synced from this directory like every dashboard in `docs/grafana/`
(**edit the JSON file, never the Grafana UI** — the host path unit re-syncs
within ~15 s).

## Enabling the exporter

Two gates must both be on:

1. **Build feature** (dev builds already carry it): `pyproject.toml`
   `[tool.maturin]` features include `degenbot-bot/hotpath-prometheus`.
   Release wheels override the feature list, so shipped artifacts have zero
   hotpath footprint. After any Rust-source change rebuild with
   `uv sync --reinstall-package degenbot` (see AGENTS.md — maturin caching
   otherwise silently ships a stale `.so`).
2. **Runtime gate**: `DEGENBOT_HOTPATH=1`. The guard is constructed by
   `BlockPump::run_with_stream` (`rust/crates/engine/degenbot-bot/src/profiling.rs`);
   hotpath starts the exporter automatically with the guard — there is no
   separate exporter flag.

```
DEGENBOT_HOTPATH=1 ./run_bot.sh start
curl -s http://127.0.0.1:6772/metrics | head
```

Standalone proof without the bot:

```
cd rust && DEGENBOT_HOTPATH=1 RUSTFLAGS=--cfg tokio_unstable cargo run -p degenbot-bot \
    --features hotpath-prometheus --example hotpath_prometheus_probe
```

## Exporter configuration

| Env var | Default | Purpose |
|---|---|---|
| `HOTPATH_PROMETHEUS_PORT` | `6772` | Exporter port |
| `HOTPATH_PROMETHEUS_HOST` | `127.0.0.1` | Bind address; the devcontainer sets `0.0.0.0` so pasta can forward host traffic |
| `HOTPATH_PROMETHEUS_AUTH_TOKEN` | unset | Token required in the `Authorization` header |
| `HOTPATH_PROMETHEUS_FAST_BUCKETS` | 250 ns - 10 s | Classic buckets for function/lock/channel/future/IO histograms |
| `HOTPATH_PROMETHEUS_SLOW_BUCKETS` | 100 us - 60 s | Classic buckets for SQL/HTTP histograms |

NOTE (devcontainer): the `-p 6772:6772` publish and `HOTPATH_PROMETHEUS_HOST`
override are CREATE-TIME fields — the running container must be rebuilt
(`.devcontainer/rebuild.sh`) before the host path can reach the exporter.

## Prometheus scrape config

Add a job to the operator `prometheus.yml` (the same file the degenbot alerts
rules load into — see [ALERTS.md](ALERTS.md)); scrape through the pod, using
the pasta gateway for the published port:

```
global:
  scrape_interval: 5s

scrape_configs:
  - job_name: hotpath
    scrape_native_histograms: true
    static_configs:
      # The ethereum-pod Prometheus shares a netns with reth/jaeger, so its
      # localhost is NOT the host — the -p 6772:6772 publish is only reachable
      # at host.containers.internal (same mechanism as the degenbot 9464 job).
      # A localhost:6772 target fails with "connection refused".
      - targets: ["host.containers.internal:6772"]
```

`scrape_native_histograms: true` negotiates the protobuf format and ingests
high-resolution native histograms (bucket ratio 2^(1/8), ~9%). Without it the
exporter serves the text format with coarse classic buckets; with it, the
`_bucket`/`_sum`/`_count` classic series are dropped unless you add
`always_scrape_classic_histograms: true`.

## Queries

The dashboard uses NATIVE-histogram PromQL:

```
# p99 function duration
histogram_quantile(0.99, sum by (function) (rate(hotpath_function_duration_seconds[1m])))
# average function duration
histogram_sum(rate(hotpath_function_duration_seconds[1m])) / histogram_count(rate(hotpath_function_duration_seconds[1m]))
# calls per second
rate(hotpath_function_calls_total[1m])
# allocation rate
rate(hotpath_function_alloc_bytes_total[1m])
# future average poll duration (sampling-correct)
rate(hotpath_future_poll_seconds_total[1m]) / rate(hotpath_future_sampled_polls_total[1m])
```

CLASSIC-variant fallback (older Prometheus, or
`always_scrape_classic_histograms`) — substitute these forms; quantiles from
the coarse log-spaced ladder are rough:

```
histogram_quantile(0.99, sum by (function, le) (rate(hotpath_function_duration_seconds_bucket[1m])))
rate(hotpath_function_duration_seconds_sum[1m]) / rate(hotpath_function_duration_seconds_count[1m])
```

Rules that hold in both modes:

- Durations are seconds; counters are cumulative since profiler start, so
  panels use `rate()`.
- With time sampling enabled, `*_total` counters still count every call while
  duration histograms only hold sampled calls — `sum`/`count` averages stay
  correct, quantiles are estimates.
- Families only appear once their instrumentation has data (no locks/channels/
  gauges instrumented yet means no series — empty panels are normal).

## Metric surface (dashboard rows)

- **Process & runtime** — `hotpath_build_info`, `hotpath_uptime_seconds`,
  `hotpath_rss_bytes`, `hotpath_threads`, `hotpath_thread_cpu_percent{,_max}`.
- **Tokio runtime** (ergo 2N6UKZ: `tokio_runtime!` in the pump loop +
  `RUSTFLAGS=--cfg tokio_unstable` on hotpath builds) — alive tasks / workers /
  global queue depth always; steals / worker-local queue / polls export when
  nonzero; the blocking pool exports once spawn_blocking work exists.
- **Function & concurrency profiling** — per-function duration p99/avg/calls/
  alloc rates, future poll duration (`assert_ws_block_complete`, the
  per-block getLogs completeness call), custom `gauge!` values
  (`detached_solve_in_flight`, `engine_registered_paths`).
- **Deferred** (instrumented-site type coupling, not gaps): channel queue
  depth needs `channel!` endpoint wrapping at the FFI boundary (the
  degenbot-python result/block channels feed typed
  `mpsc::UnboundedSender` fields in degenbot-bot); mutex wait-vs-hold
  needs the instrumented-lock type swap across
  `Arc{<}parking_lot::Mutex{<}ArbitrageEngine>>` signatures; I/O
  wrappers cannot wrap alloy transports (not our IO types).

Alert rules on these families are deliberately NOT added yet; watch a few
instrumented runs first, then move actionable thresholds into
[ALERTS.md](ALERTS.md) (currently Grafana-managed rules, ADR-040).
