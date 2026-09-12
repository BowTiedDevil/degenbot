# Controlling logging, tracing, and telemetry

The operational form of the observability standard; the binding decision record
is [ADR-043](adr/ADR-043-observability-standard.md). This is the single source
of truth for what the bot emits, at what level, and how to dial it up or down.

degenbot has two first-class consumers — the pure-Rust MEV bot and the
Python-driven bot — so the contract is split between the **core** (which emits
signals and owns no sink) and the **wiring** (which installs sinks and chooses
levels).

## The four channels

| Channel | Consumer | Default | Content |
| --- | --- | --- | --- |
| Console log | operator | on | process lifecycle and outcomes |
| Span (OTel) | investigator | when `telemetry.otel` | per-operation timing + structured context |
| Metric (Prometheus) | alerting | on | aggregates, low cardinality |
| Forensic file | repro | opt-in | full-field dumps |

A fact is emitted on exactly one channel; it is never restated as a per-event
INFO line. Any fact that paging depends on is carried by *both* the console and
a metric, because OTel can be unavailable when it matters.

The Rust core reaches Python `logging` and the stderr `fmt` writer through one
`tracing` subscriber (`init_logging_subscriber` in
`rust/crates/degenbot-python/src/python_log_layer.rs`); `log::` records are
bridged in by `tracing_log::LogTracer`. Prometheus metrics are declared in
`degenbot-bot/src/instruments.rs`.

## Levels

| Level | Use | Budget |
| --- | --- | --- |
| ERROR | abort, integrity loss, shutdown seam; mirrored by a metric | rare |
| WARN | degraded but continuing: tripwire, quarantine, retry exhaustion, verify failure, posture cordon, auto-recovering seam | O(events) |
| INFO | process lifecycle and outcomes: one block summary per block, one line per submit decision, per-phase boot/backfill/pump lines | O(1)/block + O(submits) |
| DEBUG | per-entity detail: per-pool, per-path, per-candidate, per-sim, per-log, phase timing, verify diagnostics | O(entities) |
| TRACE | per-hop/per-field dumps and raw traces | unbounded (opt-in) |

The rubric is deliberately asymmetric: if a line is per-pool or per-path, it is
DEBUG. The console firehose that motivated the standard was per-entity lines
emitted at INFO.

## Targets

The closed domain set is `degenbot::state`, `degenbot::path`,
`degenbot::solver`, `degenbot::sim`, `degenbot::pump`, `degenbot::exec`,
`degenbot::verify`, `degenbot::ingest`, `degenbot::rpc`, and
`degenbot::aave`. Every diagnostic is a DEBUG event under its domain; there
is no separate `diag` target. Engine diagnostics are emitted within an active
engine span, which is what makes them visible in Jaeger.

Span names are `degenbot.<area>.<verb>` and metric names are
`degenbot.<noun>_<unit>`. The console formatter derives its `[area]` prefix
from the target, so there is no hand-written tag to keep in sync.

## The control surface

| Knob | Class | Shape |
| --- | --- | --- |
| `telemetry.log_level` | behavior | closed enum `off\|error\|warn\|info\|debug\|trace` |
| `telemetry.diag` | verbosity | validated map `{ domain = "level" }`, ships empty |
| `telemetry.forensic` | behavior + sink | one capped, rotating file target |
| `telemetry.otel`, `telemetry.metrics_addr` | behavior | driver wiring |

`telemetry.diag` is the **console escalation** knob: it raises named domains
on the console without touching the OTel side (which already carries
`degenbot` at `debug`). Keys are validated at config load against the closed
domain set, so a typo is a boot error naming the offending key and the valid
set — never a silent no-op. The map is validated even when `RUST_LOG`
overrides it, with one WARN that it is being ignored.

### Precedence

Precedence is a branch, not an ordering:

1. If `RUST_LOG` is present it is used as-is on every sink, the config log
   knobs are ignored, and the active source is named once at startup.
2. Otherwise `telemetry.log_level` + `telemetry.diag` compile into one
   `EnvFilter` (the console filter).
3. The OTel spans layer is an independent fixed default,
   `warn,degenbot=debug`, whenever `telemetry.otel` is on.

`RUST_LOG` is directives of the form `target=level`, comma-separated, applied
most-specific-first; see
[`tracing-subscriber`'s EnvFilter docs](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html).

## Operator recipes

| I want to … | Do this |
| --- | --- |
| Quietest useful run (warn/error only) | `RUST_LOG=warn ./run_bot.sh` |
| Default operator posture | `./run_bot.sh` |
| Keep `info` but raise one domain on the console | `[telemetry.diag] sim = "debug"` (or the env equivalent) |
| Full Rust + Python debug | `RUST_LOG=debug DEGENBOT_DEBUG=1` then the bot |
| Just the Rust core at debug | `RUST_LOG=info,degenbot=debug` |
| Full diagnostic stream in Jaeger only | leave the defaults — the OTel layer already runs `degenbot=debug` |
| Prometheus scrape | `curl http://127.0.0.1:9464/metrics` (override `telemetry.metrics_addr`) |
| Jaeger traces | see the [bot-telemetry skill](../.agents/skills/bot-telemetry/SKILL.md) |

## The console writer

**Exactly one console-emitting writer per process** (ADR-043 §6). In the
Python driver the binding is present, so the Rust→Python bridge
owned by `logging.py` writes the console and the `fmt` layer is routed to
`io::sink()`; in a standalone Rust binary the `fmt` layer owns it. A record is
never written twice — the owner is *derived* from binding-present, not
switched by an env var.

The bridge's queue is **bounded** (8192 records): a stalled TTY or full pipe
cannot stall the per-log pump. A push past the ceiling drops the record and
counts it as `degenbot.log_dropped_total{sink="console"}`, so the ceiling is
visible in Prometheus rather than silent.

## Retired verbosity flags

The former per-diagnostic `DEGENBOT_*` env flags are retired (no aliases);
each maps to a domain and is now a DEBUG event under it. A boot-time WARN
scans the process environment for any retired name and prints the equivalent
domain. The mapping and the disposition of the four default-ON streams
(`verify_dbg`, `v2_calc_trace`, `dump_call_trace`,
`sim_log_reverted_swaps`) live in
[ADR-043 §5](adr/ADR-043-observability-standard.md).

`DEGENBOT_LOG_FMT` is retired with them: the console owner is now derived, so
the two-tunnel switch has no equivalent knob. Setting it warns at boot.

## Implementation status

The standard is being migrated in five phases (ergo epic `RAYW7I`). Until a
phase lands, its knob is not yet live; the tables above describe the target
contract and this section tracks the gap.

- **Phase 1** (docs + ADR) — landed.
- **Phase 2** — telemetry facade + closed domain targets landed.
- **Phase 3** — landed. Every verbosity key in ADR-043 §5 is retired; the
  probes are unconditional DEBUG (or TRACE for the forensic dumps) events on
  their domain, and the per-entity INFO firehose is demoted. Two keys were
  reclassified from the §5 retire table into the behavior-flag list:
  `state_lock.trace` and `state_lock.diag` gate diagnostic *collection* cost,
  not log emission, so they keep their boolean config (the ADR table is
  corrected accordingly).
- **Phase 4** — landed: `telemetry.log_level` (closed enum, defaults to the
  wiring default) and `telemetry.diag` (validated map, ships empty) resolve
  both record-layer filters through `degenbot_bot::telemetry::resolve_filters`;
  an explicit `RUST_LOG` is a branch that wins verbatim on every sink (the
  config knobs are ignored, with one WARN naming the active source). The
  console writer is single-owner and bounded with its drop counter;
  `DEGENBOT_LOG_FMT` is retired (boot-time WARN). `telemetry.forensic`
  remains the one unimplemented knob of this phase.
- Phase 5 — golden snapshots, metric gates, naming normalization.

## See also

- [ADR-043](adr/ADR-043-observability-standard.md) — the decision record.
- [Bot configuration keys](rust-config-keys.md) — the generated key table.
- [Bot telemetry skill](../.agents/skills/bot-telemetry/SKILL.md) — Jaeger and
  Prometheus workflows.
