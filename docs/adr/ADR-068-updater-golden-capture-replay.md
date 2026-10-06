# ADR-068: Golden capture replay is the updater coherence harness — recorded RPC cassettes, SQL statement ledgers, measured before fixed

**Status: proposed (decision recorded; implementation sequenced in ergo, deliberately held behind concurrent work).**

## Context

The pool updater and the Aave updater (survey: {doc}`../updater-rpc-sql-survey`) are
chunk loops whose only end-to-end coherence mechanism is the pre-commit chunk
verification against live RPC — a production gate. The SQL apply path is tested with
synthetic inputs; the fetch/decode/verify span is untested; and the verification gate
itself has no negative probe. RPC- and SQL-level inefficiencies (write lock held across
network round-trips, full-map read/rewrite per touched pool, per-event N+1 SQL,
unbatched revision `eth_call`s) are suspected but unmeasurable without a deterministic
replay workload.

The tree already contains the shape of the answer: `degenbot-rpc::offline`'s
`OfflineProvider` serves recorded chain JSON through a real `AlloyProvider`, the
investigation code runs "pseudo-golden" replays over committed captures, and cassette
recording has precedent (`scripts/record_curve_tripool_cassette.py`). The recorded
format lacks `eth_getLogs` — the updaters' primary RPC — and both run entries build
their provider internally from `rpc_url`, leaving no seam to inject a recorded one.

## Decision

**D1 — Golden captures live at the RPC transport seam.** A cassette records
`(method, canonical params) → raw JSON response` entries and replays through a
transport that presents as a real `AlloyProvider`; both updaters run unchanged
end-to-end over it. The recorded format extends the `OfflineProvider` JSON rather than
inventing a parallel schema.

**D2 — The recorder is Rust-side.** Consistent with the retirement of the Python
capture scripts (ergo `KSB4IW`): the recording transport and its driver entry are Rust
(Rust example/CLI), not a new Python capture script.

**D3 — SQL traffic is captured too.** A rusqlite trace/profile hook emits a normalized
statement ledger (SQL text, arg shape, rows changed, µs) and a canonical touched-table
dump per chunk apply. The golden set for one recorded span is: cassette + DB dump +
statement ledger.

**D4 — Cassettes and ledgers are machine-emitted artifacts under the drift-gate and
negative-probe idioms.** The pipeline that writes a capture is the pipeline the gate
runs (regenerate-and-diff, byte-identical ordering); every gate ships one demonstrated
red path (mutated response / mutated dump must fail).

**D5 — The provider seam is injection, not a mock server.** The run entries accept an
already-built `AlloyProvider`; the CLI and Python shells keep their behavior via thin
wrappers that build the live provider from `rpc_url` (hard cutover, no compat layer).
A localhost mock JSON-RPC server was rejected: port juggling, nondeterministic request
ordering, and CI flakiveness for zero seam benefit — the construction site is one line
per entry.

**D6 — Measure on the captures before fixing.** The replay bench and per-chunk counters
(RPC round-trips/bytes, SQL statements, write-lock hold wall time) produce the baseline;
the predicted fix order (hoist RPC out of the transaction → delta persist → apply-stage
batching → config-dispatch batching/memoization → fetch pipelining → dead-query removal)
is re-ranked against it, and each fix is gated by unchanged golden dumps plus ledger
counter drift gates.

## Considered options

- **Typed-seam golden transcripts only** (`ChunkInputs` → write-set): kept as a
  complement for SQL logic, rejected as the primary — it never touches fetch, decode,
  or the verification gate.
- **Forked-node replay in CI**: rejected for determinism and the anvil `eth_callMany`
  failure (`/skill:node-identity`); retained as the capture-generation environment.
- **EVM-oracle generation** (ScratchEvm/frame-replay): a strong wave-2 source of
  adversarial captures and gate negative probes; too large for the first cut.
- **Differential testing vs the legacy Python updater**: rejected — against the
  two-consumer architecture and the hard-cutover policy.

## Consequences

- Wave-2 catalog notes (from the generator's own findings): a zero-amount `Mint`
  cannot originate from a real pool (`require(amount > 0)`) — the skipped-write
  branch's capture is manufactured through a real `LOG4`; and negative tick
  spacing is unsound in the real pool (the pool constructor accepts it, only
  the factory validates, and compressed-tick invariants invert on a plain
  down-swap). Both are recorded in wave-3 task `HGOV5W` with their capture plans.

- The coherence gate becomes testable in CI: a mutated recorded slot must roll the
  chunk back (and leave `last_update_block` unadvanced) in a test, not first in prod.
- Fixture maintenance is bounded: cassettes re-record only when the pinned span
  changes; the drift gate keeps recorder and captures from diverging.
- The performance program becomes evidence-driven: every fix lands with a measured
  before/after from the same workload, and the ledger counter gates keep the N+1
  shapes from returning.
