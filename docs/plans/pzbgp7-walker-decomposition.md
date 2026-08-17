# PZBGP7 / T1 (SCMSTK) — walker structural split + ADR-031 record correction: DONE

- `derive_plan` (was one fused 3,849-line fn) is now a 29-line `(len, repay-sequence)`
  dispatcher over six enclosure-block modules: `grammar_walker/shapes/{all_v2_chain,
  two_hop_seed_v4, two_hop_v4_led, three_hop, two_hop_uniswap_only, tag_residual}.rs`.
- Pure code move; byte-identity verified: executor suite (109 lib + parity/golden suites)
  + degenbot-simulation + degenbot-backrun-strategy all green; clippy clean.
- ADR-031 header + consequences corrected in place (the "tag-derived, no prot-tuple"
  claim was a spelling check, not a structural one — ~24 `facts[i].prot` if-branches
  remain); CONTEXT.md Enclosure/Hop-facts entries updated to match.
- New structural pin: `tests/walker_shapes_layout.rs` (hygiene invariant; stays past T2).
- Note for T2: the stale doc comment `/// PLACEHOLDER_REVERT_START T1 (v3_v4_v3)` above
  `DERIVE_PLAN_CALLS` refers to epic 62V6Q5's T1 — delete it with the counter.

## T2 (BMY6JJ) — tripwire deletion: DONE

- Deleted: `DERIVE_PLAN_CALLS` static + increment + stale `PLACEHOLDER_REVERT_START` doc;
  `DONE` allowlist counter probe; both `include_str!` source scans (match-arm count,
  per-family deriver count) and the `EXPECTED_*` consts.
- Kept: `d6_enclosure_derived_from_facts` (V4 hops carry `Repay::NetZero`), now documented
  as the single load-bearing invariant, reading facts through `family_facts`.
- Fork resolved: `family_facts` survives as the *future single facts dispatcher*
  (T3 folds the twin `build_for_walk` 30-arm match into it, not vice versa) —
  re-documented accordingly.
- Gates: clippy 0; executor (111 tests incl. layout pin + tag invariant) +
  simulation (78) + backrun-strategy (100) all green.

## T3 (UPBUE6) — twin-table/delegate collapse: DONE

- grammar_walker.rs 3,537 → 872 (-927); grammar_shape.rs +49 (inline validator gate).
- facts_for: all-V2 fast-path → 5 explicit override arms → per-variant hop_facts fallback.
- build_walk: the single Plan pipeline (public — degenbot-simulation tests import it).
- build_plan_bytes/BuildPlan kept #[cfg(test)] for the Reject-path test.
- VERIFIED DEVIATION (recorded): facts_of_v3v4v3's missing arity guard is a semantic
  compression — a 4+-hop [V3,V4,V3,X] path previously produced 3-hop facts (dropping a
  hop, uncaught); it now Declines. Siblings all had the guard; drop was unreachable from
  production (2/3-hop paths) and the old behavior was a bug per ADR-030.
- All pure facts fns uniform; cleanest case — quote: "None. All 30 matched the pattern."
- Gates: 122 executor + 186 (sim+backrun) green; clippy 0 across the three crates.

## T4 (RQQIUK) — tag-row walker spike: PARKED (negative)

Findings: docs/spikes/rqgiuk-tag-row-walker.md.

Core evidence:
- 23 3-hop arms in three_hop.rs partition into 3 structural groupings; only the V2/V3-only 7-arm
  set is cleanly tag-actuable without axis invention.
- v3v4v2 vs v3v4v4 (same Repay sequence) differ in trailing-swap mechanics + cb order, which are
  prot-keyed by code, not by ledger algebra; distinguishing them via facts requires >=2 new axes
  (TerminalForm, TrailingHopAmounts, plus a Leading/Middle/Terminal Position axis) — the bespoke
  shape vocabulary ADR-029 D4 was designed to avoid.
- Estimate: ~15 of 23 arms are incrementally unifiable (7 V2/V3-only + 8 V4-crossing); the remaining
  8 need position-role machinery whose cost exceeds the fan-out it removes.

Epic outcome: T1 (split) + T2 (tripwire deletion) + T3 (facts_for/build_walk collapse) delivered.
ADR-031 record now honest: dispatch is (len, repay-sequence) gated over per-shape modules; ordering
defects are caught by the LedgerValidator + revm matrix. T4 documented the cost of going further.

## T5 (AE4ZK2) — terminal-form axis: DELIVERED

- Axis: `HopFacts.terminal_form: Option<TerminalForm>` (2 values), set once in `facts_for`
  for the [V3, V4, V2|V4] terminal position; None everywhere else by construction.
- Merged arm: three_hop.rs v3v4v2+v3v4v4 -> one body diverging only on trailing mechanics +
  cb step ORDER (a derivation of mechanics, not an enumerated arm).
- Discipline honored: exactly one axis; no second HopFacts field added; the brief's
  "if it needs >2 values or a second axis, park" tripwires both unused.
- Red->Green evidence: terminal_form_routes_the_v3v4_pair; goldens/parity/honesty/layout
  + simulation/backrun matrices all green; clippy zero.
- Remaining 21 three_hop arms consume None (compile-enforced default) — future absorption
  opportunities documented in docs/spikes/rqgiuk-tag-row-walker.md §What IS extractable.

## T6 (A5AC77) — topology rule walker spike (ANALYSIS) + Group A (v2/v3-only) — IMPLEMENTED

Analysis: docs/spikes/t6-topology-rules-analysis.md — 3-rule rule-set + 2 new facts needed only
for 2 group-C holdouts. Group A (7 arms, three_hop.rs L17-480) collapsed into rule_walk_v2v3
(246 lines): shadow byte-identity across 7 families x 4 configs, then hard cutover. The V2-flash
upgrade rule needed one shadow-caught refinement (V2-led-only gating; naive version over-triggered
on v3v3v2). three_hop.rs: 2,203 -> 2,150. All gates green.

Next per T6-impl plan (WV4BUA): T6b (9 v4-led arms) + T6c (6 remaining + 2 holdouts consuming
repay_mechanism / seed_delivery axes).

## T6b (v4-led) — IMPLEMENTED, zero holdouts
9 v4-led arms (incl. v4v4v4's V4Batch) collapse into rule_walk_v4_led; even the batch layout
proved data-driven. NO new facts: the v4-led legs are derivable from prot + currencies alone.
R1 exception: v4v2v3's terminal V3 flash wraps the V4Unlock. Gates green; three_hop.rs kept
roughly flat (2120) since arm code turned into walker + shadow docs.

## T6c (mixed group + holdouts) — IMPLEMENTED, epic closed
7 remaining arms collapse into rule_walk_v2v3_v4_mixed. The analysis's 2 holdouts land the 2
predicted facts: `seed_delivery = V4TakeCompact` (v2v3v4), `repay_mechanism = AutoFromExecutor`
(v3v2v4). Facts scoped exactly like `terminal_form`: None default, set by facts_for only for
consuming families, walker reads them (never None-panics).

END STATE: zero hand-authored per-family bodies in three_hop.rs. The remaining structure:
facts_for (per-variant facts) -> derive_plan (shape gates incl. terminal_form axis) -> the 3
rule walkers -> LedgerValidator gate -> plan_to_bytes.

File totals commit-by-commit: three_hop.rs 2203 -> 2150 (T6a) -> 2120 (T6b) -> 2279 (T6c),
grammar_walker.rs 961 -> 1058. ~1560 lines of bespoke arms replaced by ~900 lines of walker +
shadow tests, with the ADR-029 D4 data-vs-code split now fully honored: no arm enumerated by
prot; every family is facts-set + walker-read.
