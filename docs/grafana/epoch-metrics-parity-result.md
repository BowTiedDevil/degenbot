# T5 final validation result (epoch-stage dashboard metrics, epic 4QTLNG)

## Metric-name parity (instruments <-> dashboard <-> alerts)
- Model: unit-aware rendering of all 55 instrument declarations in
  rust/crates/engine/degenbot-bot/src/instruments.rs (counters -> _total,
  histograms -> _seconds/{,_bucket,_sum,_count} by unit; By -> _bytes;
  unit "1" -> _ratio; weiless histograms unsuffixed).
- Compared against ALL 106 PromQL target exprs in docs/grafana/degenbot-overview.json
  and docs/grafana/provisioning/alerting/degenbot-grafana-rules.yml
  (62 distinct degenbot_* families referenced).
- VERDICT: PASS - zero unmatched families.
- Retired-name mentions confined to prose successor notes (panel/ALERTS
  descriptions explaining the retirement): degenbot_block_header_to_solved,
  degenbot_drain_queue_depth, degenbot_drain_queue_wait_seconds. No query
  target references any retired family.

## Rust gates
- cargo test -p degenbot-bot --lib: 645 passed, 0 failed (default build).
- cargo test -p degenbot-bot --lib --features otel: 671 passed, 0 failed
  (incl. new epoch-race contract test epoch_race_renders_and_retired_family_gone).
- cargo clippy -p degenbot-bot --all-targets (default + otel): 0 errors.

## Structure gates
- degenbot-overview.json: python3 -m json.tool OK.
- degenbot-grafana-rules.yml: YAML_OK, 12 rules (2 new epoch rules present).
- Docs sweep: no operator-facing doc instructs against the new surface
  (historical ADR/measurement docs left intact by design).
