# ADR-031: The executor grammar as a facts-driven Plan walker — per-protocol hop facts + mechanics, derived enclosure

**Status: accepted; implemented (A1–A3 of epic `62V6Q5` complete) and D6 **realized** (epic `6SU5LM`).**

> **D6 (epic `6SU5LM`) realization:** every one of the 35 `build_*_walk`
> family producers is now a **thin delegate** — `derive_plan` routes every
> family through `facts_of_<family>` + the per-shape derivers, and **no
> `build_*_walk` body emits a `PlanStep` directly** (the D6 honesty invariant,
> enforced by the `facts_driven_tests` probe + `honesty_invariant`). Structural
> and behavioral parity is pinned by the revm contract matrix, the
> `spike_derivation` golden suite (all 28 green), and the full executor +
> simulation suites. The combined gate (T8) is green.

## Context

`grammar_shape.rs` is a 7,013-line monolith of 30 hand-written per-family Plan
producers (`build_*_plan`), dispatched by the 30-row `build_for`/`AxisSupport`
table. Each re-derives the ordering invariants by hand, so correctness is gated
only *post-hoc* by the `LedgerValidator`; the D0 defect class (V4-take-before-
credit, terminal-V2 1-wei overdraw) escaped the hand-authored producers and was
caught only by the revm matrix. ADR-029 D4 chose per-family Plan authoring as
the interim mechanism and deferred a generic walker (6ZIE5X a-branch); CM5V3X
costed it. The corpus — 30 builders + `AxisSupport` + validator + the
25-family revm matrix — now exists to generalize over and regress against.

## Decision

Adopt the hybrid deepening: the grammar becomes **per-protocol hop facts**
(data) + **per-protocol mechanics** (code) + **one generic walker** that derives
enclosure and emits a single `Plan`. The encoder (`plan_to_bytes`) and the
validator gate (`plan_to_ledger_ops` + `LedgerValidator`) are **reused
unchanged** — both are pure functions of the Plan, so the walker's only output
contract is "a Plan". `build_for`/`AxisSupport` dissolve into hop facts
(family axis-support becomes a fact, not 30 rows). Most per-protocol mechanics
already exist as shared, byte-identical helpers (`v4_scaffold_table`,
`v4_bridge_steps`, `v4_terminal_capture_steps`, `funding_branch`, `enc_v*`).

Landing was feature-flag parallel (A1/A2, `--features walk`) gated by **byte-
identity to the hand-written producers on every family**, then a hard cutover
(A3). The cutover is complete: the walker is now the **sole producer** — the 30
`build_*_plan` bodies, `build_all_v2_chain`, and the `build_for`/`AxisSupport`
rows are **deleted**, and `family_axis_support` is **facts-derived** from the
hop-protocol patterns rather than a 30-row table. Correctness is gated by the
**revm contract-matrix** (execution against the on-chain `cmd_executor`, per
ADR-029 D5 — not byte-parity against the suspect producers, which no longer
exist), plus the golden-byte corpora and honesty invariant. A validator
`Reject` remains always-fatal (ADR-030).

## Considered options

- **Fully per-family Plan authoring** (status quo, D4 interim): keeps ordering
  hand-reasoned per family — the adversarial surface this epic deletes.
- **Per-family declarative trace tables**: data, but still ~30 rows, one per
  family — doesn't kill the combinatorial fan-out D6 targets.
- **Walker without a mechanics seam** (all data): blurs D4's data-vs-code split
  and can't express imperative Solidity callback wiring. Rejected.

## Consequences

- A new protocol is one hop-facts descriptor + one mechanics module (D6
  additive proof), never a per-family body.
- Enclosure derivation makes the take-before-credit / terminal-V2-draw classes
  unrepresentable, not merely caught.
- `grammar_shape.rs` shrank from 7,013 lines to ~1,600 (the shared mechanics
  helpers + dispatch + derive seam + tests); the per-protocol facts table and
  walkers live in `grammar_walker.rs`.
- `Reject` stays reachable (amounts from solver inputs + hand-authored facts
  can still err), so the validator and ADR-030's fatal-Reject remain load-bearing.
