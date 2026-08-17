# T5 — Design + land terminal-form axis: make the V4-crossing set expressible

## Goal
Reopen the T4 spike's negative finding on explicitly-recorded user direction: the cost of
inventing one new HopFacts field is accepted; bespoke dispatcher remains the target for removal.

## Decisions this task makes
1. What the new axis is. Candidates:
   - **TerminalForm::{DirectHandoff, UnlockInternal}** (what the trailing hop's swap step is —
     this is what distinguishes v3v4v2 from v3v4v4 AND justifies the cb-order flip).
   - vs. a wider FundingForm axis covering consumed_inputs[2] access.
   Choose by writing the rule it drives; the axis earns its place iff one stated rule
   replaces one arm-family (two or more concrete arms collapse behind it).
2. Where the axis lives. Must reach HopFacts (it's already carried through facts_for); the
   grammar_shape entry sets it for the terminal hop.
3. Scope of absorption. Target the pair (v3v4v2, v3v4v4); out of scope are the ~6
   V4-crossing arms with genuinely different wrap logic (v4v2v4 vs v4v2v2 differ in V2
   placement relative to the unlock). Enumeration rule: list which arms the axis absorbs,
   every one, before coding.

## Red
A row-style test for the two target families asserting:
- they no longer appear as prot-tuple if-branches in three_hop.rs,
- one shared arm body handles both (or the shape dispatcher routes them by axis alone).

## Green
- Byte-identity: goldens + revm matrix green for both families over the shared body.
- The axis shows up in grammar_ledger / walker facts construction exactly once; the rest of
  the walker reads it, never re-derives it from prot.
- Clippy clean; executor crate stays green (122 tests base after T3).

## Non-goals / guards
- Adding the axis does not admit new bespoke dtype. If the axis has to distinguish more
  than two values for the two target arms alone, that's the bespoke-taxonomy trap reopening;
  stop and record.
- ADR-029/031 are NOT reverted: this completes D6's ambition in a way the record already
  anticipates ("one new descriptor + one mechanics module per protocol"), and the ADR text
  acknowledges ordering defects are caught, not theoretically unconstructible.
- Cost guard: if the real implementation requires more than one new axis, the spike's
  negative verdict was right — park again with evidence.

## Deliverable
One of: (a) a new commit(s) landing the shared body for v3v4v2+v3v4v4 with axis-driven
dispatch + gates green, or (b) a written suspension documenting exactly which axis was
tried, where it failed, and the rule that could not be stated.