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
