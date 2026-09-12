# ADR-043: The observability standard — four channels, a level rubric, one target taxonomy

**Status: accepted** (2026-09-12, ergo epic `RAYW7I`; migration phases `N57KBH` -> `Y6TP27` -> `A44VW4` -> `ISESRO` -> `2PDJAL`).
Settled in the project-wide logging/tracing/telemetry audit and reviewed by two
peer sessions (kimi-k3, glm-5.3-flash) across two passes. The review caught
three over-designs — a numeric verbosity ladder, a `degenbot::diag` marker
namespace, and a `scope:level` string grammar — recorded under *Alternatives
considered*, and two contradictions in the first draft (a `diag` default that
would re-flood the console, and domain keys outside the closed set), now fixed.
**The user sign-off checkpoint is satisfied (2026-09-12):** the level rubric,
the target taxonomy, and the control surface are approved; the migration
phases may proceed.

## Context

The Rust core, the Python driver, and OpenTelemetry each carry their own
observability conventions, and the seams between them have no shared
vocabulary. Four planes were in play:

1. **Rust `tracing`** — forwarded to Python `logging` through
   `tracing_log::LogTracer` + `PythonLogLayer`
   (`rust/crates/degenbot-python/src/python_log_layer.rs`), plus an optional
   stderr `fmt` layer. Inventory: ~121 `info!`, 74 `warn!`, 50 `error!`,
   35 `debug!`, 55 `info_span!`, 6 `tracing::instrument`.
2. **Python `logging`** (`src/degenbot/logging.py`): ~58 `info`, 25
   `debug` call sites.
3. **OTel / Prometheus** (`degenbot-bot/src/otel.rs`, `instruments.rs`):
   ~90 metric instruments, service `degenbot-bot`, span export only (there is
   no OTel logs bridge).
4. **~25 ad-hoc `DEGENBOT_*` verbosity flags** in the `trace`,
   `simulation`, and `aave` config tables, each with a private default and a
   hardcoded emit level.

### Evidence

A real run (`logs/bot_run.log`) was dominated by per-entity lines emitted at
`info` outside the console-capped target:

```
406 [verify-dbg]        398 [pool]/[path]      318 [sim]
242 [sim-revert-swap]   169 [state]             120 [solve-phase]
 96 [sim-trace]          85 [sim-fail]           78 [sim-verify]
```

The load-bearing diagnosis: **verbosity was chosen at the call site** (a
bespoke env flag plus a hardcoded level) instead of at the sink. The correct
mechanism already existed — the `degenbot::diag` target capped at `warn` on
console and uncapped on the OTel layer — but only 27 sites used it against 112
INFO sites.

Defects surfaced by the audit: `src/degenbot/logging.py` documented a retired
`pyo3-log` bridge; 72 `println!`/`eprintln!` calls in `rust/crates/*/src`
outside examples and tests (several on production failure paths);
`block_pump.rs` printed in production; the two-tunnel console
(`DEGENBOT_LOG_FMT`) could emit a record twice at different levels.

## Decision

### 1. Four channels, one fact per channel

| Channel | Consumer | Default | Content |
|---|---|---|---|
| Console log | operator | on | process lifecycle and outcomes |
| Span (OTel) | investigator | when `telemetry.otel` | per-operation timing + structured context |
| Metric (Prometheus) | alerting | on | aggregates, low cardinality |
| Forensic file | repro | opt-in | full-field dumps |

A fact is emitted on exactly one channel; it is never restated as a per-event
INFO line. **Sanctioned asymmetry:** any fact that paging depends on must be
carried by *both* the console and a metric, because OTel can be unavailable
when it matters.

**No channel may carry credentials.** URL userinfo and API-key path segments,
auth headers, and key material are forbidden in console, span attributes,
metric labels, and forensic dumps alike. RPC URLs are redacted at their
`Display` boundary by a dedicated type; a field-level deny-set
(`key`, `secret`, `token`, `auth`, `authorization`) applies at every emit
site. Forensic dumps carry calldata, never transport auth.

### 2. Level rubric (cardinality-bounded)

- **ERROR** — abort, integrity loss, or the shutdown seam; every ERROR class
  also emits a metric so a sustained condition cannot be invisible. A failure
  seam that auto-recovers (e.g. a WSS disconnect while reconnecting) is WARN,
  never ERROR.
  - **Crash path.** A panic hook emits exactly one ERROR carrying the panic
    payload and thread/task name, then `record_exception`. Every spawn
    boundary wraps and joins its task and logs the `JoinError` payload — not
    a bare "task failed". `record_exception` sets the active OTel span's
    `Status::Error`, so the Jaeger view of a failure seam is complete rather
    than a log line beside a green span. This is the class of the current
    `eprintln!` abort in `fleet_solve_executor.rs`.
- **WARN** — degraded but continuing: tripwire, quarantine, retry exhaustion,
  verify failure, posture cordon, auto-recovering seam.
- **INFO** — process lifecycle and outcomes only. Per block: one block
  summary, O(1). Per phase (not per block): boot, backfill start/end, pump
  start/stop. Per submit decision: one INFO line at the exec seam, O(submits)
  where submits are rare relative to blocks — "we attempted to take money"
  deserves a console line even when OTel is unavailable, with the span event as
  the attributed record.
- **DEBUG** — everything O(entities): per-pool, per-path, per-candidate,
  per-sim, per-log, phase timing, verify diagnostics.
- **TRACE** — per-hop/per-field dumps and raw traces.

### 3. One target taxonomy; diagnostics live inside an engine span

Targets are `degenbot::<domain>` for a closed domain set:
`state`, `path`, `solver`, `sim`, `pump`, `exec`, `verify`,
`ingest`, `rpc`, `aave`. Diagnostics are DEBUG events under their own
domain — there is **no** separate `degenbot::diag` marker namespace, and no
ad-hoc sub-scopes: the closed set is the only set. Per-domain escalation is
`diag = { sim = "debug" }`.

Invariant: **engine diagnostics are emitted within an active engine span.**
The OTel layer is spans-only
(`OpenTelemetryLayer::new(tracer).with_context_activation(false)`), so an
event reaches Jaeger only as a span event on an active span. This invariant —
not a target — is what makes the diagnostic stream trace-visible, and it
replaces what the rejected marker pretended to guarantee. Enforcement: a facade
`debug_assert`/one-shot WARN when a diagnostic fires with no current engine
span, plus a golden-snapshot assertion that demoted events appear as span
events in the exported trace.

### 4. Typed control surface

| Knob | Class | Shape |
|---|---|---|
| `telemetry.log_level` | behavior | closed enum `error\|warn\|info\|debug\|trace` |
| `telemetry.diag` | verbosity | validated map `{ domain = "level" }`, ships **empty** |
| `telemetry.forensic` | behavior + sink | one capped, rotating file target |
| `telemetry.otel`, `telemetry.metrics_addr` | behavior | driver wiring |

**Precedence is a branch, not an ordering:** if `RUST_LOG` is present it is
used as-is on every sink, the config log knobs are ignored, and the active
source is named once at startup. Absent, `log_level` + `diag` compile in one
code path into one `EnvFilter`; the compiled directive string is an
implementation detail, never the contract. `diag` keys are validated at
config load against the closed domain set from §3, so a typo is a boot error
naming the offending key and the valid set — never a silent no-op. The map is
validated even when `RUST_LOG` overrides it, with a single WARN noting that it
is being ignored, in the same detection-not-compatibility spirit as the
retired-name scan.

**The two per-layer defaults are explicit.** The compiled `EnvFilter` is the
*console* filter; the OTel record filter is an independent fixed default
(`warn,degenbot=debug`), not a second compilation of the `diag` map.
Per-domain OTel throttling via `diag` is explicitly out of scope for this ADR.

- **Console filter** — the wiring default (`INFO` for the Python driver,
  `WARN` for the Rust bot) plus any operator `diag` directives. `diag`
  ships empty, so the default console posture is quiet.
- **OTel spans layer** — `warn,degenbot=debug` when `telemetry.otel` is on,
  so domain diagnostics reach Jaeger by default.

`diag` is therefore the operator's explicit **console escalation**; the OTel
side is already uncapped for degenbot domains.

**Sampling and retention.** The OTel layer uses parent-based `AlwaysSample`
while `telemetry.otel` is on: the preserve-by-traces mechanism depends on the
full diagnostic stream being exported, and cost is bounded by block frequency
rather than by volume. Retention is owned by the collector configuration, not
by the core. No head-sampler may be improvised in-process, because it would
gut the preservation guarantee.

### 5. Behavior flags stay; verbosity flags retire

**Behavior flags** keep their boolean config: `otel`, `metrics_addr`,
`ws_completeness`, `sim_exit_on_fail`, `pump_debounce_ms`, `hotpath`. The two
`state_lock` diagnostics (`trace`, `diag`) also stay: they gate diagnostic
*collection* cost (acquire-time backtrace capture; per-read-hold bookkeeping),
not log emission, so they are behavior flags rather than verbosity knobs. The
always-on slow-hold WARN is unaffected by either.

**Verbosity flags retire hard** (no aliases). Each maps to exactly one closed
domain or to the forensic sink, so the migration is mechanical and auditable:

| Retired key | Domain | Disposition |
|---|---|---|
| `verify_dbg` | `verify` | DEBUG; default-TRUE stream |
| `v2_calc_trace` | `sim` | DEBUG; default-TRUE stream |
| `sim_log_reverted_swaps` | `sim` | DEBUG; default-TRUE stream |
| `sim_divergence_log` | `sim` | DEBUG |
| `dump_call_trace` | forensic | full-field dump; default off |
| `dump_tick_maps` | forensic | full-field dump; default off |
| `ws_trace` | `ingest` | DEBUG |
| `drain_dbg` | `pump` | DEBUG |
| `trace_dispatch` | `pump` | DEBUG |
| `trace_register_seed` | `path` | DEBUG |
| `trace_liquidity` | `state` | DEBUG |
| `trace_tick` | `state` | DEBUG |
| `gate_trace` | `solver` | DEBUG |
| `aave_evtrace` | `aave` | DEBUG |
| `aave_tx_trace` | `aave` | DEBUG |

Four of these default **TRUE** today
(`rust/crates/degenbot-config/src/schema.rs`: `verify_dbg` :315,
`v2_calc_trace` :225, `dump_call_trace` :211,
`sim_log_reverted_swaps` :328). They are **not** preserved by seeding the
`diag` map — that map is the console escalation knob, and seeding it would put
the 406-line verify stream straight back on stderr, defeating this ADR. They are
preserved on the **Jaeger side** by the OTel record-filter default
(`degenbot=debug`, §4): the console stops flooding and the diagnostic signal
survives in traces. Full-field dumps belong on the forensic target and default
off. **The console behavior for these four streams changes by default, and that
change is deliberate.**

Migration safety net: a boot-time WARN scans the process environment for any
name on the closed retired list and prints the equivalent domain/directive. This
is **detection, not compatibility**; the shim is scoped to the retired-name list
and is deleted at 0.7 alongside the other gated cleanups.

### 6. Library hygiene and one console writer

Core crates use `tracing` only: no `println!`/`eprintln!` on production
paths, no level policy, no subscriber installation. Libraries own no console.
Only *wiring* installs, via `try_init` so a host subscriber always wins. The
standalone Rust bot wiring defaults to `WARN`; the Python driver defaults to
`INFO`.

**Exactly one console-emitting writer per process** — `fmt` layer *or*
`PythonLogLayer`, never both. The `DEGENBOT_LOG_FMT` two-tunnel hack is
removed; the owner is derived from binding-present. `logging.py`'s stale
`pyo3-log` docstring is corrected in the same pass.

The console writer is **non-blocking**: a bounded queue drained by a dedicated
writer task, so a stalled TTY or full pipe cannot stall the per-log pump. Drops
are counted as `degenbot.log_dropped_total{sink}` — a silent ceiling is not
acceptable. On shutdown, telemetry providers flush **before** the tokio runtime
is torn down (the panic path excepted, where the best-effort flush is
documented), so the tail of a trace is not lost to teardown ordering.

### 7. Naming, derived not written

Span `degenbot.<area>.<verb>`; metric `degenbot.<noun>_<unit>`; target
`degenbot::<domain>`. The message `[area]` tag is deleted: the console
formatter (`_AreaFormatter` in `src/degenbot/logging.py`) renders
`LEVEL [area] message` with the area derived from the record's logger name —
the domain target for a bridged Rust record, the owning segment for a
Python-side one — so maintainers set one thing and a record that moves between
areas needs no message edit. Landed in `4QYTPH`; a production message that
re-introduces an `[area] ` tag fails `observability_naming.rs
::messages_carry_no_area_tags`.

### 8. Enforcement

Primary enforcement is compile-time:

- `clippy::disallowed_macros` with exact paths
  (`tracing::info`, `tracing::debug`, `tracing::warn`, `tracing::error`,
  `tracing::trace`, and `tracing::event`) in workspace lints, allow-scoped to
  the telemetry facade module. Banning the level macros without
  `tracing::event` would leave `event!(Level::INFO, …)` as a trivial bypass.
  Call sites use facade macros (`diag!(domain = …)`, `op_info!(…)`) that force
  a target.
- Span creation (`info_span!`, `tracing::instrument`) stays available, but the
  boundary is named: spans follow the §7 naming and the §3 engine-span
  invariant, and are covered by the golden snapshot. Facade `op_span!`
  wrappers are preferred where a hot path needs a static handle.
- `clippy::print_stdout` / `clippy::print_stderr` denied at library crate
  roots; the pure-Rust binary opts out where output is the product.

Secondary gates:

- a grep gate for direct `std::io` writes and Python logging misuse;
- a behavioral volume gate: a capture subscriber over a fixture block asserts
  steady-state INFO ≤ N per block and that every INFO+ record matches the
  allowlist;
- a **metric cardinality gate**: a per-instrument label allowlist, and the
  series-count self-metric from §9 watched by an alert;
- golden snapshots of boot, one block, and one revert that capture console
  text **plus metric series and exported span events**.

### 9. Metric cardinality

Metric label value sets are `&'static` closed constants, following the
existing `telemetry::error_kind` / `error_reason` pattern. Pool ids, path
ids, block hashes, and other unbounded values never appear as metric labels —
they belong in spans and logs. A `degenbot.metric_series` self-metric exposes
the live distinct-series count so cardinality blowup is visible before the
collector falls over. This is the metric-side twin of §2's level cardinality
bounds.

### 10. Signal-preservation acceptance criterion

Every demoted or deleted log site must, in its diff, name the metric constant /
span attribute / span event that preserves the tripwire, or carry an explicit
`no preserving signal, rationale: …` clause. The golden snapshot captures
metric series and span events, not only console text, so a demotion that
silently blinds a detector is a visible diff. This is the difference between a
logging refactor and an observability regression.

## Migration

1. This ADR plus a rewrite of `docs/logging.md` (the operational form of the
   standard); fix the stale `pyo3-log` docstring.
2. Ship the telemetry facade macros, the clippy lints, the engine-span
   `debug_assert`, and the panic hook.
3. One mechanical per-crate pass: demote per-entity INFO to DEBUG, adopt
   `degenbot::<domain>` targets, delete verbosity flags per the §5 table —
   every diff carries the signal-preservation clause.
4. Add `telemetry.log_level`, the validated `diag` map, the two explicit
   filter defaults, the boot-time retired-flag detection, the non-blocking
   single console writer with its drop counter, and remove production prints.
5. Golden snapshots, the metric cardinality gate, the series-count self-metric,
   and naming normalization for spans and metrics.

## Consequences

- Console output becomes operator-grade: per-entity detail moves to spans and
  metrics, while the four previously default-ON forensic streams remain
  available in traces by default (OTel record filter `degenbot=debug`).
- The ~17 verbosity env vars collapse to a typed two-knob surface plus one file
  sink; every removed name is detected at boot rather than failing silently.
- Metric cardinality and log volume gain gates, so regressions are diffs rather
  than surprises; console drops are counted, never silent.
- The core crates become genuinely library-safe for a `cargo add degenbot`
  consumer: no sink, no level policy, no direct writes.

## Alternatives considered

- **Numeric verbosity ladder (k8s `-v`).** Rejected: a third encoding of the
  same knob alongside `log_level` and `diag`; k8s needs it because it lacks a
  structured filter language, which EnvFilter provides.
- **`degenbot::diag::<domain>` marker namespace.** Rejected: the OTel layer
  is spans-only, so target-level markers never conferred trace visibility;
  visibility is span membership. Kept as a one-line revisit note: if a
  deliberate OTel **logs** bridge lands (e.g. for standalone systemd
  operation), marker-target filtering may become meaningful again.
- **A raw `log_directives` EnvFilter string.** Rejected: unvalidated string
  in a typed schema fails at runtime rather than at config load.
- **`scope:level` string micro-grammar.** Rejected in favour of a validated
  map that compiles to `EnvFilter` internally.
- **Seeding `diag` to preserve the default-TRUE streams.** Rejected: `diag`
  escalates the console, so a seed would re-flood stderr; preservation is a
  property of the OTel record filter.
- **Aliasing retired verbosity flags.** Rejected per the project's hard-cutover
  rule; replaced by boot-time detection of the retired names.

## References

- Prometheus, *Metric and label naming* — app prefix, base units, `_total`
  suffix, cardinality warning.
- OpenTelemetry, *Logs data model* (severity mapping) and *Metrics API*
  (`unit` as a first-class field).
- Envoy, `--component-log-level component:level` — per-component levels.
- Kubernetes, *Logging conventions* + KEP-1602 — `V(n)` ladder, structured
  key/values, "shared libraries should not log errors themselves".
- Git, `api-trace2` — unified targets replacing ad-hoc `GIT_TRACE_*` flags.
- Rust `tracing` / `log` crate guidance — libraries emit, never install a
  global subscriber.
- CPython `logging` HOWTO — "WARNING and greater … is regarded as the best
  default behaviour" for a library.
