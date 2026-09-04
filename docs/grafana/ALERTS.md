# degenbot alerts (T5, epic RMH23E)

**Install:** a ready-to-load rules file ships alongside this doc -
[`degenbot-alerts.yml`](degenbot-alerts.yml). Add it to `prometheus.yml`:

    rule_files:
      - /path/to/degenbot-alerts.yml

then restart/reload Prometheus. Check status at `http://<prometheus>:9090/alerts`
(INACTIVE = healthy; PENDING = condition true, `for` window elapsing; FIRING = page).
The expressions below document what each rule watches and why; label values
match `instruments.rs` exactly (`skipped_broadcast_failed`, `expired`,
`error`, ...).

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

### Recorded failures (epic D63GSE)
    expr: sum(rate(degenbot_errors_total[5m])) > 0
Any failure surfaced through `telemetry::record_exception` (solver-state
desync, WS log drop, sim failure, submit/monitor failure, verify mismatch,
drain stall) increments `degenbot_errors_total{kind=...}`. The `kind` label is
a CLOSED SET (`telemetry::error_kind`) — triage with e.g.
`degenbot_errors_total{kind="sim_failure"}`. Full context lives in Jaeger:
filter traces by tag `error=true` (service `degenbot-bot`); the failed span
carries an `exception` event with `exception_type` / `exception_message`.
The `DEGENBOT_FAILURE_MODE` env var selects whether the bot exits (default),
quarantines-and-continues (`harden`), or just continues (`continue`).

### Placeholder-series gotcha (`init="true"`, incident 2026-08-22)
Metric attributes are Prometheus LABELS: a one-shot "marker" observation
(e.g. `blocks_observed.add(0, {init="true"})`) creates a PERMANENT second
time series frozen at zero. `rate(...[1m]) == 0` matches that series
forever, so the header-stall alert fired continuously on a healthy bot.
The stall rules now carry `{init!="true"}` guards, and the marker touch was
removed from `instruments.rs` — never add marker-attribute observations to
shared instruments.

## Notes


- Import `docs/grafana/degenbot-overview.json` for the companion dashboard;
  point it at the Prometheus data source scraping the bot endpoint.
- Traces: Jaeger UI, service `degenbot-bot`. Per-block traces root at
  `degenbot.pump.block`; solves nest via the drain-pipe span propagation
  (`DrainWork` carries the dispatch-time span).



## Alerting surface (ADR-040, DO5Q5E/UI7QNH) - CUTOVER COMPLETE 2026-09-04

All alert rules now run as **Grafana-managed unified alerting**, evaluated by
Grafana 13 itself and visible in **Alerting > Alert rules** (folder Degenbot):
[provisioning/alerting/degenbot-grafana-rules.yml](provisioning/alerting/degenbot-grafana-rules.yml)
is the declarative copy (datasource UID inlined; deploy = POST the rule groups via the
/api/v1/provisioning/alert-rules API or mount the file into provisioning/alerting/).

degenbot-alerts.yml in this directory has been DELETED - do not re-add a
rule_files entry for it (running both surfaces concurrently double-fires
every alert). If a running Prometheus still references it, remove the entry
and reload Prometheus at the operator convenience.

Rule inventory (10 rules, 2 groups, 60s evaluation):

### Critical (page immediately - money or liveness at risk)

- **DegenbotProcessDown** (NEW - the abort blind spot):
      expr: sum(up{job="degenbot"} == bool 1) < bool 1   # noDataState=Alerting
      for: 2m
  Covers ANY bot death, including ADR-040 exit-stance desync aborts and fatal
  buckets. The OLD header-stall alert deliberately ignored down targets; this
  one owns them. State semantics normalized with the bool comparison so a
  healthy bot reads Normal, not NoData.

- **DegenbotHeaderStall**: rate(blocks)==0 while target up. Healthy-state
  normalized: every comparison rule ends `or vector(0)` so Grafana reads
  Normal instead of NoData.
- **DegenbotDrainerStall** (drain_queue_depth >= 2 for 5m).
- **DegenbotStateHeadDivergence** (abs(state_head_lag) >= 3 for 1m).

### Warning (degraded but not bleeding money)

- **DegenbotErrorRate** - any recorded failure kind in 5m. solver_state_desync
  now means a QUARANTINE fired (ADR-040 default): open the newest
  logs/desync/desync-*.json for the reproduction state; the bot is still
  trading its other paths.
- **DegenbotDesyncQuarantine** (NEW) - dedicated quarantine signal
  (degenbot_quarantine_events_total + the errors-kind), so a containment is
  visible as its OWN event, not buried in the any-error rate.
- **DegenbotSubmitFailures**, **DegenbotMonitorExpirySpike** (>50% expired
  15m), **DegenbotProfitEfficiencyDrop** (>80% left on table),
  **DegenbotSimulateErrorRate** (>10% error outcomes 10m) - severity
  semantics unchanged from the Prometheus file.

### Removed

- **Apply-miss rate** - there was never an alert on it; the panel was removed
  with the dashboard rework (benign classes dominated the signal).
- **DegenbotHeaderStall job matcher**: guards up{job="degenbot"} == 1 so a
  stopped bot does NOT fire it - DegenbotProcessDown owns absence.
