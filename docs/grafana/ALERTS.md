# degenbot alert suggestions (T5, epic RMH23E)

Prometheus rules matching the metrics exported by `degenbot-bot` (scrape
endpoint: `DEGENBOT_METRICS_ADDR`, default `127.0.0.1:9464`). All expressions
assume the `degenbot_` prefix families from `src/instruments.rs`.

## Critical (page immediately - money or liveness at risk)

### Header stall (the bot is blind)
    expr: rate(degenbot_blocks_observed_total[1m]) == 0
    for: 2m
Headers stopped arriving; the pump cannot see new blocks. First observed live
2026-08-21: the dev reth WS feed went silent after ~2 blocks while the chain
kept advancing - this alert would have fired within 2 minutes.

### Drainer stall (solve/dispatch not advancing)
    expr: degenbot_drain_queue_depth >= 2
    for: 5m
A persistent backlog means the solver cannot keep up or the drainer froze.
(The Rust-side B3 backstop aborts at 30s; this catches the slow-degradation
case before the abort.)

### State head divergence (freeze signature, ergo 3YA7ZJ)
    expr: abs(degenbot_state_head_lag_blocks) >= 3
    for: 1m
Pool state and engine clock disagreeing by multiple blocks is the pre-condition
of the historical drain-freeze incident.

## Warning

### Submit failures
    expr: sum(rate(degenbot_submit_outcomes_total{outcome="skipped_broadcast_failed"}[10m])) > 0
RPC/broadcast path degraded.

### Monitor expiry spike
    expr: sum(rate(degenbot_monitor_outcomes_total{outcome="expired"}[15m]))
      / clamp_min(sum(rate(degenbot_monitor_outcomes_total[15m])), 0.001) > 0.5
Half of submitted txs never confirming - fee/nonce/pool-contention problem.

### Profit efficiency drop
    expr: degenbot_profit_missed_total
      / clamp_min(degenbot_profit_realized_total + degenbot_profit_missed_total, 1)
      > 0.8
More than 80% of found profit is being left on the table (unsubmitted).

### Simulate error rate
    expr: sum(rate(degenbot_simulate_verdicts_total{outcome="error"}[10m]))
      / clamp_min(sum(rate(degenbot_simulate_verdicts_total[10m])), 0.001) > 0.1

## Notes

- Import `docs/grafana/degenbot-overview.json` for the companion dashboard;
  point it at the Prometheus data source scraping the bot endpoint.
- Traces: Jaeger UI, service `degenbot-bot`. Per-block traces root at
  `degenbot.pump.block`; solves nest via the drain-pipe span propagation
  (`DrainWork` carries the dispatch-time span).
