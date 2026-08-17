# ADR-031: The executor grammar as a facts-driven Plan walker — per-protocol hop facts + mechanics, derived enclosure

**Status: accepted; implemented (A1–A3 of epic `62V6Q5` complete) and D6 **realized** (epic `6SU5LM`).**

> **D6 realization (post re-drive + enclosure-derivation migration), as
> corrected 2026-08-16 (arch review epic `PZBGP7`, task `SCMSTK`):** all 35
> `build_*_walk` family producers route to a single `derive_plan`
> post-refactor, and the hand-written per-family `derive_2hop_*` /
> `derive_3hop_*` / `derive_all_v2` *bodies* are eliminated. But the earlier
> "enclosure is derived from `Repay`/`OutDest` tags, NOT prot-tuple match
> arms" claim overstated what shipped. The *dispatch* is facts-keyed — a
> `(len, repay-sequence)` partition — but the enclosure bodies are
> hand-authored per shape: ≈24 `facts[i].prot == Prot::…` if-branches
> dispatch between shapes' internal variants (including the 3-hop block's
> ~17-arm per-family enumeration). Only the single-V4-middle residual is a
> genuine `Repay`/`OutDest`-tag partition. V4 hops do carry
> `Repay::NetZero` (via `v4_hop_facts_netzero`) and that fact *is* load-bearing
> for the residual partition. The misreading came from the citation of a
> source scan that counted literal `match` arms while the same dispatch
> existed as if/else chains — a spelling check, not a structural one.
>
> Structural correction (same epic): the fused ≈3,849-line `derive_plan` is
> now six shape modules under `grammar_walker/shapes/` (`all_v2_chain`,
> `two_hop_seed_v4`, `two_hop_v4_led`, `three_hop`, `two_hop_uniswap_only`,
> `tag_residual`) behind a ≈29-line dispatcher on `(len, repay-sequence)` —
> one module per enclosure block, dispatcher keyed on exactly what the
> blocks already key on.
>
> **Consequence of the correction:** enclosure ordering defects are
> *caught*, not unrepresentable — correctness rests on the revm contract
> matrix + golden suites (per ADR-029 D5, the designated source of truth)
> plus `LedgerValidator`'s always-fatal Reject (ADR-030). Whether the
> `Repay`/`OutDest` tag vocabulary can genuinely absorb the enumerated
> shapes (i.e. make D6's "derived" claim literally true) is spike
> `RQQIUK`; until it reports, treat "tag-derived enclosure" as aspiration
> for the enumerated shapes, not fact.
>
> **Resolution (same epic, tasks T5+T6 — the aspiration is now realized):**
> tags alone could not absorb the shapes (spike `RQQIUK`, negative on the
> merged-pair holdout), but the vocabulary could: the `terminal_form` axis
> merged the "blocked" `v3v4` pair, and the topology-rule analysis found
> 21/23 arms derivable from a 3-rule debt-flow set, with the last 2 unlocked
> by the `repay_mechanism` + `seed_delivery` facts. (The design docs behind
> both — `docs/plans/pzbgp7-terminal-form-axis-draft.md` and
> `docs/spikes/t6-topology-rules-analysis.md` — were removed in the stale-docs
> cleanup `71ec78b2`; their findings survive in this paragraph, the
> `CONTEXT.md` walker glossary, and the rule walkers in
> `grammar_walker/shapes/three_hop.rs`.) All 23 3-hop bodies are deleted;
> three rule walkers (`rule_walk_v2v3`, `rule_walk_v4_led`,
> `rule_walk_v2v3_v4_mixed` in `grammar_walker/shapes/three_hop.rs`) derive
> the enclosures from the facts, byte-identical (golden suites + revm matrix
> green; the shadow-walk pin tests caught three rule corrections pre-cutover).
> D6's "enclosure is derived from facts, NOT chosen" claim is now literal.

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
- Enclosure ordering defects are caught, not unrepresentable: the
  validator gate + revm matrix are the enforcement (see the header's
  record correction — the per-shape bodies are hand-authored code; the
  "derived" claim applies fully only to the residual tag partition).
- `grammar_shape.rs` shrank from 7,013 lines to ~1,600 (the shared mechanics
  helpers + dispatch + derive seam + tests); the per-protocol facts table and
  walkers live in `grammar_walker.rs`.
- `Reject` stays reachable (amounts from solver inputs + hand-authored facts
  can still err), so the validator and ADR-030's fatal-Reject remain load-bearing.
