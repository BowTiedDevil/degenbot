# ADR-083: The pipeline census — one event vocabulary behind one dispatch door

**Status: accepted.** Drafted at the architecture-review grilling (candidate 1;
rounds 1–3). Deepens the drain-path telemetry seam without touching ADR-043's
channel split or ADR-041's stage table.

## Context

The drain path's Prometheus census was a shallow module: `instruments.rs`
exposed ~70 `observe_*/count_*/set_*` methods, and the census knowledge — which
metric fires at which pipeline point, and what the values mean — lived at ~200
`if let Some(p) = crate::instruments::pipeline()` call sites across ~18 files
plus the `degenbot-arbitrage` and `degenbot-submission` consumers. Adding or
moving one metric row edited the instrument struct *and* every emit site; the
None-guard was re-written at each site. The module's interface was as wide as
its implementation, and the deletion test condemned it: the metric map existed
only in the callers.

## Decision

**D1 — A closed census event vocabulary.** `src/census.rs` (feature-independent:
both the otel-gated instrument set and the default-build stub share it) defines
`CensusEvent`, one variant per instrument answer. Variant names are drawn from
the pipeline's own vocabulary — the ADR-041 stage table, the Batch outcome
record, `GLOSSARY.md` — never from the metric an event feeds. Label payloads
are `&'static str`: a label value that could not be `&'static` is the
`tests/metric_cardinality.rs` gate's refusal arriving at compile time.

**D2 — One door, guard folded inside.** `instruments::record(event)` is the
sole emit entry: it resolves the lazy instrument set (the `pipeline()`
None-guard is inside, one branch per observation — the same discipline as
`log::debug!`) and dispatches byte-identically onto the unchanged instrument
bodies. The `pipeline()` accessor is census-module-private; the default-build
stub gains a no-op `record` so both builds share one call surface.

**D3 — The seam is pinned structurally.** `tests/census_gate.rs` fails the
build if `pipeline()` is named outside the census module: an emit site can no
longer silently reintroduce a metric name or a per-site guard.

_live-invariant gate (ADR-087 D2): watches the live `pipeline()` accessor in `degenbot-bot/src/instruments.rs`; not tripwire precedent._

**D4 — Metric names are frozen and parity-pinned.** Dashboards and alerts
query metric strings verbatim (the PROMETHEUS NAME COUPLING notes).
`tests/metric_name_parity.rs` pins the constructed-name set bidirectionally:
renaming a metric without moving its dashboard/alert consumers fails the gate
in the same edit.

**D5 — Two-step discipline.** Step 1 (this change) is a byte-identical 1:1
mapping — every pre-census method call becomes exactly one event; no metric is
renamed, merged, or re-scoped. Step 2 starts EMPTY: any merge of near-twin
metrics (the log-arrival trio, the two lock-timing homes) must first prove the
twins are one fact, and move the dashboard + alert consumers in the same
change.

**D6 — The `PhaseSpan` drop-guard is deferred, not abandoned.** Every
intra-frame duration site today attributes its label AFTER the measured region
(`cycle_outcome.arm_label()` post-dates the hold it times), so a guard would
need a mutable label slot before any site can use it — a seam with no adapter.
The guard lands with the first site whose label is available at region start;
until then `record` carries the value.

**Scope.** `PipelineInstruments` only. `UpdaterInstruments` keeps its own
smaller seam; the trace-span channel stays in `stage_telemetry.rs` and its
siblings (ADR-043: one fact per channel).

## Consequences

Emit sites read `crate::instruments::record(crate::instruments::CensusEvent::…)`
and know no metric. The event→instrument map, the label sets, the bucket
boundaries, the None-guard, and the dashboard couplings live in one module;
the `GLOSSARY.md` **Pipeline census** entry names the concept. Adding a metric
is one variant, one match arm, one frozen name — and the gates catch every
shortcut.

## Reopen condition

If the `record` dispatch table becomes a choke point (frequent conflicts, or
the `too_many_lines` expectation grows contentious), split the event enum into
sub-vocabularies along the stage-table lines — but keep the one door.
