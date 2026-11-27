# ADR-109: Feature-gated test lanes are mechanically inventoried and filtered

**Status: accepted (implemented).** (2026-11-27; ergo card `BD23UW`.) The gate
lives in `scripts/feature_lane_inventory.py`, its dispositions in
`scripts/feature-lane-dispositions.txt`, and the lane command in
`just test-rust-feature-lanes` (CI: the `rust-test` job's feature-gated step).

## Context

A cargo feature can remove a test from every routine lane without removing it
from the tree. `required-features` skips a whole target, `#![cfg(feature = ...)]`
skips an integration file, and `#[cfg(feature = ...)]` on an item or module
removes it from the lib test binary. A lane that "enables the feature" is
therefore only as good as its feature list, and a hand list of what that list
covers goes stale at the first new gated target -- silently, because the
missing tests do not fail.

The observability family had that gap in both directions. `degenbot-bot/otel`,
`degenbot_rs/otel`, and `degenbot-arbitrage/otel-replay` gate every telemetry
integration test, and no routine gate enabled them, so the OTel layer's span,
metric, snapshot, and replay contracts compiled only on demand. Turning the
feature on naively is worse than the gap: the same feature gates

- the live Jaeger E2E probe (`tests/otel_jaeger_e2e.rs`), which needs an OTLP
  collector and Jaeger's query API and returns early when
  `DEGENBOT_JAEGER_E2E` is unset -- an offline lane would report a green skip;
- the TLS destructor repro (`tests/otel_tls_repro.rs`), whose non-ignored child
  test deliberately leaks an OTel `ContextGuard` so the test process aborts.

The statement-ledger surface has the same shape: four
`degenbot-fixture-tools` `[[bin]]` targets are gated behind `sql-ledger` and no
routine gate compiled them, while the ledger bench that measures the same code
is a measurement, not a gate.

## Decision

**D1 -- One mechanical inventory.** `scripts/feature_lane_inventory.py`
enumerates the workspace from cargo metadata (each target's `required-features`)
plus a module walk (file-level `#![cfg]`, module `cfg`s, item `cfg`s, and the
full test path nextest will print). Gates are parsed into predicate trees and
evaluated against the lane's feature set with `test` on; platform predicates
(`unix`, `target_os`, `target_arch`, `target_pointer_width`) are answered by the
host that builds the lane. Any unmodeled predicate is a finding, checked on
EVERY feature-relevant unit before the enabled filter runs: a positive unknown
predicate evaluates false and would otherwise demote the unit to out-of-lane
without a word. Scope is the lane's own feature set: a test gated on a feature the
lane does not enable cannot appear in that lane's binaries, so it is printed as
out-of-lane and remains another lane's claim (ADR-084 D3 corpus lanes, the
policy lanes, the tool lanes, the fork lane). Nothing is hand-listed to become
visible or invisible.

**D2 -- Four classes, reconciled both directions.** Every in-lane unit is
`offline`, `live`, `ignored`, or `measurement`. `offline` is the default, so a
new offline test joins the lane with no edit. Every other class is a row in
`scripts/feature-lane-dispositions.txt` naming the unit and its owning ADR, and
the gate fails when a row names a unit the tree no longer has. Two mechanical
rules keep the default honest: an item carrying `#[ignore]` must be classified
`ignored`, and a binary that reads a live gate env var (`*_E2E`, `*_LIVE`) must
name every one of its items, because default-to-offline is exactly how a live
probe becomes a green skip.

**D3 -- The filter is generated, not written.** The recipe's nextest `-E`
expression is printed by the same script from the same classifications. There is
no second selection to drift: the filter that runs is the filter the gate
reconciled. Every term is scoped by the exact binary id --
`binary_id(=package::target)` for a whole binary and
`(binary_id(=package::target) and test(=name))` for one item -- because a bare
`test(=name)` filters the name globally and would also suppress an offline twin
that reuses the name in another binary.

**D4 -- The selection is proven in both directions.** The recipe captures the
nextest list with and without the generated filter and reconciles both against
the inventory: the filtered capture must select every offline unit and no
excluded one, and the unfiltered capture must FAIL the same reconciliation. That
negative probe is what distinguishes "the filter excludes the live probe" from
"the live probe stopped existing".

**D5 -- One offline feature-gated lane.** `just test-rust-feature-lanes` runs
the workspace with the canonical fixed features (`capture-fixtures`,
`cache-lab`, `executor-ab-probe`, `test-harness`) plus `degenbot-bot/otel`,
`degenbot_rs/otel`, and `degenbot-arbitrage/otel-replay`, under the generated
filter. CI's `rust-test` job runs it after the golden replay, so the OTel and
replay contracts are gated offline without pretending to be live-verified.

**D6 -- The SQL-ledger tool lane is a compile check; its bench stays
on-demand.** `just check-dev-tools fixture-sql-ledger` joins the `rust-lint` CI
job as compile-only coverage of the four gated `[[bin]]` targets. The ledger
bench (`degenbot/benches/updater_replay_bench`, `just bench-updaters`) is
ADR-068 D6 measurement and remains an on-demand run, never a gate.

**Dispositions owned by ADR-043.** The live Jaeger E2E legs are `live`: only an
operator with a collector and Jaeger's query API runs them, and their absence
from the offline lane is explicit in the dispositions file. The TLS destructor
repro is `ignored` at binary granularity: its child is expected to abort, so no
batch lane may run it. ADR-043 records both dispositions beside the OTel layer
they exercise.

## Consequences

- A new feature-gated test cannot be invisible: it either joins the lane's
offline set automatically or fails the gate because its binary is live-gated and
unnamed, its `#[ignore]` is unclassified, or its gate uses an unmodeled
predicate.
- A retired gated test needs its disposition row removed in the same change;
the reconciliation fails on the stale row. This is a live-invariant gate (ADR-087
D2): it watches present units, not a removed-name list.
- The lane is an execution gate, not a coverage percentage: it settles *which*
gated tests run offline and proves that selection, and leaves live verification
to the operator lane it names.
- The inventory reads the tree, not the build: a source that fails to compile
still enumerates, and the nextest capture then fails on the missing unit.

## Alternatives considered

- **A hand list of gated targets in the justfile.** Rejected: it is exactly the
  artifact that goes stale silently, and `required-features` already exists to
  be read.
- **`--all-features` for the lane.** Rejected: mutually exclusive and
  measurement-only feature families would be unified into one build; the
  workspace's lanes are named on purpose (ADR-084 D3).
- **`--run-ignored all` plus per-test skip logic.** Rejected: an aborted
  subprocess is not a test verdict, and a self-skipping live probe is a green
  that proves nothing.
- **Leaving the OTel tests on demand.** Rejected: their span/metric contracts
  are offline-testable and were simply unreachable.

## Evidence

- `scripts/feature_lane_inventory.py` -- inventory, dispositions, generated
  filter, two-sided `verify` with `--negative-probe`.
- `scripts/feature-lane-dispositions.txt` -- the two ADR-043 exclusion rows plus
  the offline helper inside the live-gated binary.
- `tests/test_feature_lane_inventory.py` plus `tests/fixtures/feature_lane/` --
  the committed-tree reconciliation, the generated filter, both reconciliation
  directions, the negative probe, and the fail-closed refusals.
- `just test-rust-feature-lanes` -- the lane and its post-run proof.
