# `6ZIE5X` decision memo — how to deliver ADR-029 D4 for V4 / 3-hop

**Status:** finding + recommendation, awaiting branch confirmation before any
ADR-029 edit. Surfaces a third option the task's (a)/(b) framing omits.

## What the audit confirmed

`6ZIE5X` was opened because "the V4 fold" is byte-faithful **transcription**,
not the `ShapeClass`+`HopFacts` byte-**derivation** the `6YUNQN` spike proved on
the V2/V3 2-hop slice. Re-reading the five 2-hop V4 families
(`derive_2hop_v4v4/v4v3/v3v4/v4v2/v2v4`) against
`docs/plans/executor-v4-ledger-rules.md`, the audit confirms:

- The ~30 transcribed functions are **not arbitrary**. Each is a fixed recipe of
  (per-hop swap mechanics) × (boundary classifier) × (capture/funding/enclosure
  rules). The boundary model in the rules doc — V4→V4 = internal ledger move;
  V4→outside = `TAKE(cur,recipient,amt)`; outside→V4 = `SYNC+TRANSFER+SETTLE`;
  native↔WETH at boundaries = `WETH_DEPOSIT`/`WITHDRAW`; trailing
  `SETTLE_ALL` — is exactly what every V4 family body implements.
- The variation across families is **predictable from the axis choice**, not
  bespoke. So a data-driven *deriver* is feasible (Branch A is real work, not
  impossible work).

## Why neither (a) nor (b) is right

**Branch (b) — re-scope D4 to "transcription"** forfeits the epic's reason for
existing. The terminal validation `VIXQYH` (D6) requires proving a new protocol
composes as **one axis value, not a combinatorial multiply** of adapters. Under
(b), every new protocol = new per-family transcriptions = combinatorial
fan-out — the very disease ADR-029 (and ADR-025 before it) was created to kill.
`VIXQYH` becomes self-refuting. So (b) is a re-scope of the *whole epic's
purpose*, not just D4.

**Branch (a) — genuinely ShapeClass-drive the V4 byte emitter** over-reads D4.
D4 does **not** require the *byte stream* be derived from declarative data. Its
actual deliverables are: (1) per-protocol **ledger facts as data**, (2) a
**generic validator** over those facts proving ordering for every
(protocol × funding × capture × bribe) combination, (3) swap/callback
**mechanics as code** behind a per-protocol interface. The `6YUNQN` spike's
"derive the bytes from `ShapeClass`+`HopFacts`" framing conflated *derivation*
with D4; D4 is about **validation from data**, with **emission as code**.

The GCC6I6 task already half-saw this: *"the validator operates on the
`LedgerOp` IR (decoupled from bytes, D5), so the next fold (WAYDTL) should route
production encoders' DECISIONS through this IR rather than validating raw
bytes."*

## Option (c) — faithful reading: data-driven VALIDATION, code-driven EMISSION

- **Emitters stay as per-protocol code** (byte-proven, transcribed). The
  `grammar_shape.rs` header's "derived" language is corrected to
  "transcribed bytes; ordering proven by the validator over declarative facts."
- **D4's deliverable is the generic validator as gate.** Each emitter emits a
  `LedgerOp` trace (the declarative ledger facts) alongside its bytes; the
  matrix runs every stream through `LedgerValidator` and asserts it passes.
- **`VIXQYH` proves additivity at the facts+mechanics layer**: a new protocol =
  new `LedgerOp` variants + new descriptor + new per-protocol mechanics impl;
  the validator auto-proves ordering for the new combinations. No new
  per-family transcription is needed for the stub row. This makes D6 true.

### What (c) requires that is NOT yet built

The current `LedgerOp` IR covers only the **two bug-class invariants** GCC6I6
targeted (PM credit-before-debit; pair-handoff seed-before-`SwapCalc`). To be
the *exhaustive* gate D4 promises, the IR must be widened to model the full
stream — V3 flash-callback credit, `ERC20_TRANSFER`, WETH deposit/withdraw,
`V4_SYNC`/`SETTLE`/`SETTLE_ALL` netting, `V4_BATCH`, capture-vs-credit `MINT`,
native ledger — and the validator must consume every production stream's trace
at matrix time. This is the real foundational D4/D5 work, and it is **no-regret
regardless of (a) vs (c)**: even under (a) you'd want it.

## Recommendation

Adopt **(c)**. Concrete consequences:

1. `6ZIE5X` → `done` via an ADR-029 clarification: D4 "derived" refers to the
   **validator reasoning over declarative per-protocol facts**, not
   byte-derivation; emitters are code mechanics. No claim that V4 bytes are
   data-derived.
2. Foundational implementation work = **widen `LedgerOp` + wire
   `LedgerValidator` as a matrix-enforced gate over every emitted stream**
   (closes the validator gap; required under both A and C; satisfies D5's
   "validator … correctness gate for every future composer/encoder change").
3. `WE45KC` carries axes as runtime per-path values; the validator enforces
   terminal-V2 pre-fund + D0 for every combination (this finally closes
   `2PT5HH`'s in-scope families).
4. `VIXQYH` proves additivity at facts+mechanics via stub Vault/lender ledgers.
5. `WAYDTL` close-out = fold all-V2 + delete hand-written adapters + the
   `cutover` backstop (pure dedupe; the transcribed `derive_*` become the sole
   emitters, now validator-gated).

(c) preserves the epic's purpose, avoids a multi-week byte-deriver rewrite that
D4 doesn't actually mandate, and still delivers the additive claim that makes
ADR-029 worth having.

## Refined additive claim (operator clarification on `6ZIE5X`/C)

A new protocol composes as **one or more axis values**, not as a new pool
grafted into every slot of the existing matrix. Concretely:

- **In scope (additive):** Balancer's Vault = one new `Ledger::External`
  value + one per-protocol mechanics impl + its own declarative ledger facts
  (+ new `cmd_executor` primitives/new `LedgerOp` variants only where the math
  is genuinely different). A representative row with one new-value×one existing
  protocol executes against a stub and passes the validator. Bounded to *that
  protocol's* layer; the validator proves ordering generically from the facts.
- **Explicitly out of scope (the explosion we reject):** "place a Balancer pool
  at hop 1 / 2 / 3 of `v2_bal_v4`" and every (new-protocol × every-slot ×
  every-neighbor × funding × capture) cell. That multiplier is the bespoke-
  adapter disease the epic kills; the additive proof must never rebuild it.

This matches VIXQYH's existing acceptance ("composes across the protocol shapes
it applies to … a representative row executes"; "old model would have needed N×
adapters"). Recording it here so a later agent cannot read "additive" as "graft
the new value into every matrix position."
