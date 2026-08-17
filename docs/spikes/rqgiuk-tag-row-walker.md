# Spike RQQIUK: can the Repay/OutDest tag vocabulary absorb the 3-hop 17-arm enumeration?

**Verdict: PARK (negative) — SUPERSEDED.**
**POST-EPIC NOTE (epic PZBGP7 complete, 2026-08-17):** The T4 PARK verdict
was overturned on input direction. T5 (terminal_form axis) collapsed the
v3v4 pair; T6 (topology rules + 2 new facts) collapsed all remaining 3-hop
arms. Read docs/plans/pzbgp7-walker-decomposition.md sections T5/T6 for
the delivered design. This document retained as the group-A foundational
analysis; the hand-authored-arms world it describes no longer exists.
 The two subtrees are disjoint algebraically; a
tag-row walker for all 23 3-hop arms would require two plus three new axes —
the exact bespoke-shape vocabulary ADR-029 D4 exists to kill.

## Method
Compared all 23 arms in `three_hop.rs` by structural signature (step vocabulary
per arm, callback composition, top-level enclosure shape) rather than by per-arm
body reading. Full signature table archived in `docs/plans/pzbgp7-walker-decomposition.md`.

## The 23 arms partition into three disjoint structural groupings

| Group | Arms (grep line range) | Signature | Can tags distinguish their enclosure? |
|---|---|---|---|
| V2/V3-only (no V4) | 17–479 (7 arms: all 3-hop combinations of v2/v3) | pure-FlashSwap streams, nested V3 flashes over V2Swap* tails + Erc20Transfer refunds | **Yes — cleanly.** AllFlash + SelfRefund/Offstream per position gives the nesting; the 7 arms are 3 Flash-wrapping permutations.
| V4-crossing, V4-not-terminal mixed | 676–1015, 1096, 1195 (8 arms: v4v4v4, v4v2v2, v4v2v4, v4v3v3, v4v3v4, v4v4v2, v4v4v3, v4v2v3, v4v3v2) | V4TakeCompact-driven, V4Unlock encloses a V4-side closure; mixed V2 leading hops attach as FlashSwap at the top level | Mostly **Yes**. Uniform signature; the enclosure is V4-centric (V4Unlock wraps all the V4 work; the V2 arm bottoms out in v2_flash). A tag row keyed on NetZero positions + V2-trailing FlashSwap-to-SelfRefund covers seven of the eight without new axes.
| V4-middle, V3-terminal | 2037, 2135 (2 arms: v3v4v2, v3v4v4) | tag_residual-like: SelfFund + V4Sync + v3_flash_to outermost; V4Unlock inside; trailing V2Swap or double-V4Swap inside the unlock's inner; Erc20Transfer repay-to-a in the cb, ordered relative to the unlock | **Partially.** The pair shares scaffolding AND differs ONLY in: (a) trailing swap (V2 terminal vs V4-terminal — reads NetZero uniformly on V4, so the *arm's existence* isn't tag-distinguishable), (b) trailing-hop funding (grants consumed_inputs[2] access only in v4v4v4-style, c-in), (c) cb step order (V4Unlock-then-repay vs repay-then-V4Unlock). |

## Why the V4-middle × v3-terminal pair is the blocker

Both v3v4v2 and v3v4v4 arms see identical Repay sequences (SelfRefundLeading +
NetZero mid + SelfRefund terminal via `v2_hop_facts`).

The arm split is determined by *swap mechanics* of the terminal hop (V2 direct
handoff vs V4 unlock-internal swap), which is prot-keyed bytecode wiring, not
ledger algebra. ADR-029 D4 calls this out: "Mechanics are code" (the ending
swap of a stream is not an ordering fact).

A tag-row walker needs to *discriminate these two arms on facts alone*, then
derive: cb order must flip. The only" clean" extension is a `Position::Terminal`
or `TerminalForm::{DirectHandoff,UnlockInternal}` HopFacts field, plus a
`TrailingHopAmounts::{ConsumedInput,Intrinsic}` field. Those are real axis
inventions, and they'd be inflated *only* to serve two arms — anti-additive.

Orthogonal complication: any tag-row walker wants a `Position` axis
(Leading/Middle/Terminal) to disambiguate same-prot hops; that's one new axis
on its own (two new axes total). Whether that's a legitimate axis (HopPosition
analog to Prot) or the bespoke-taxonomy trap ADR-031 D4 forbids (a way of
denoting families by role rather than composing mechanics) is the fork that
keeps this PARKed rather than GREEN.

## What IS extractable cheaply (leave for T5-if-any)
- **V2/V3-only 7-arm set**: trivially unifiable behind an AllFlash tag-pattern
  (V2 direct + SelfRefund terminal) — ~500 lines collapse to one arm.
- **8 of the ~14 V4-crossing arms** share v4_scaffold_table prefix + V4Unlock
  wrap and differ only in trailing-hop composition; a row walker over
  NetZero-position + trailing-SelfRefund covers them.
- The remaining 6+2 arms genuinely differ in which hop wraps which, which is
  exactly what nested enclosing structure is hard to express without either
  enumerating the row table nearly as big as the match block or inventing
  position-wise axes.

## Recommendation
Do NOT extend the axis vocabulary. The 17-arm tail is cheaper to read than to
generalize. Extract the easy 7+8 if ever there is a fourth DEX family to serve;
until then, per-shape nests with the ledger validator gate are the honest shape.
