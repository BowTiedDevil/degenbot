# ADR-069: Reopening ADR-060 D4's preserved entry surface — the prepared hop becomes one value

**Status: accepted (2026-10-09). Predecessors: ADR-060 (esp. D4), ADR-015 (the resolve→solve seam), ADR-059 (D6: hot loops stay monomorphic).**

## Context

"One CL hop's prepared tables" exists as **four representations** of the same facts:
`ResolvedHop::V3`, `ClSolveTables`, the `PreparedHop` tuple
(`(Arc<ClCrossingTable>, Arc<ClProfileTable>)`, `cl_cache.rs:44`), and `ClPathSimHop`.
Each names part of what the walker reads; none carries the whole.

The solve entries take parallel positional slices — sequences on one side, prepared
tables on the other — whose pairing invariant the types cannot express. It is defended
at runtime instead, by the per-hop **table gate**: a `walk_path_fingerprint`
re-derivation and `matches(seq)` check before the walk uses any table
(`cl/entries.rs:147-153`). The composition memo's key folds the same fields a third
time. The governing invariant — "the fingerprint is what the walker reads" — lives in
prose across three files, pinned by golden-hash tests that break on any
mixing-constant change.

The cost is paid on the hot path: with a memo active, replays skip the gate, but a
miss pays the per-hop re-derivation; and the invariant remains a comment, not a type.

## Decision

**D1 — ADR-060 D4's preserved entry surface reopens.** The four `cl` entry signatures
and the `ClSolveTables` signatures are back in scope for change. D4's preservation was
a sequencing guard, not a permanent boundary; the deepening work has arrived.

**D2 — The prepared hop becomes ONE value.** A single prepared-hop value carries the
tick-range sequence + the `Arc`'d crossing table + the word profiles + the derivation
fingerprint, folded **once at derivation**. The projection constructs it, and
`ResolvedHop` carries it inside the hop.

**D3 — The entries take one slice of a borrowed hop view.** The solve entry takes
`V2(&IntHopState) | Cl(&PreparedClHop)` — one slice, one representation. The
positional-slice pairing problem disappears by construction.

**D4 — The table gate is deleted.** The pairing it defended is structural once the
prepared value and the sequence travel together; the runtime re-derivation and
`matches(seq)` check become dead weight.

**D5 — `walk_path_fingerprint` goes crate-private** behind the value's `fingerprint()`.
The 128-bit fold body is byte-identical; only its call site moves — invoked once, at
derivation, rather than re-derived per gate.

**D6 — The memo key derives from the same prepared values the walk consumes.** The
fingerprint source and the walk source are the same value, so the key describes exactly
what the walker read by construction, not by convention.

## Why this is safe despite D4

ADR-060's interface collapse (`PieceView`) is untouched — the per-family step
simulators stay, and `PieceView` remains the walk's interface. The 128-bit fold
function is byte-identical; only its call site moves. Acceptance is ADR-060 D2's own
bar: byte-identical `WalkOutcome.result` across the solver-vs-sim parity corpus, plus
the walkmemo cross-block replay A/B.

## Consequences

- The golden-pin / verbatim-copy fingerprint tests are replaced by a **mutation
  sweep**: perturb every field `PieceView` reads, and the composed key must change.
  The property replaces the pinned constant.
- `ClSolveTables` and the `PreparedHop` tuple are deleted; their facts fold into the
  one prepared value.
- `ClPathSim` is explicitly **out of scope** — its hop shape is a separate question
  with its own evidence.
- The "fingerprint = what the walker reads" invariant stops being prose across three
  files and becomes a property of the value: the key and the walk are computed from the
  same fields.
- The stored per-hop fingerprint retired with the table gate it fed: D5's fold body stays
  byte-identical and crate-private, and the value no longer carries a copy.
