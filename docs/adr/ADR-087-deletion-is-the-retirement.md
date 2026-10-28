# ADR-087: Deletion is the retirement — retirement tripwires are rejected

**Status: accepted** (2026-10-28; reaffirms and enforces ADR-047 D4).

## Context

A retirement in this repo used to end with a deletion. Over the cutover era it came to
end with a deletion **plus a guard**. A census taken 2026-10-28 (six-section sweep of
`rust/`, `tests/`, `src/`, `docs/`, and the commit-gate layer) found **189
retirement-tripwire artifacts** — tests, gates, and prose prohibitions whose purpose is
keeping a deliberately deleted thing gone:

- 57 retired-name text greps, 45 structural gates, 41 absent-API assertions, 23
  count-guards, 12 commit/push/CI gates, 11 doc-only prose bans ("do not revive",
  "do not re-land", "do not rebuild").
- Roughly 30 ADRs own at least one; **78 of the 189 cite no owning ADR at all**.
- The negative-probe idiom (GLOSSARY.md) demands that a gate's red path be demonstrated
  once before a green run counts as evidence; 41 of the 189 show that demonstration, and
  for the rest it was never shown or is unverifiable from the file.

ADR-047 D4 (accepted 2026-09-12) had already settled the principle: *"Resurrection-guard
tests are rejected. Once a module is deleted, the compiler is the guard... Source-scan
tests are for LIVE invariants only."* Practice overruled it: every cutover after ADR-047
added guards anyway. The failure was mechanical, not philosophical — the policy lived
only inside ADR-047's body. No AGENTS.md line carried it and nothing enforced it, so
each cutover's author (human or model) found a hundred-plus examples of prior art and no
rule. Prior art outvoted written policy 189 to 1.

That dynamic is the real defect, worse than the count. Agents infer conventions from
examples, so ungoverned tripwires multiply exactly where the repo is most active
(cutovers); each is permanent by default (no kill condition); most cannot prove they
fire; and the whole layer teaches the next agent that "retire a module, leave a guard"
is the repo's convention. The convention question was forced by review of the ADR-064
five-map tripwire: an individually defensible gate that is indefensible as a pattern.

The enforcement standard this ADR holds itself to: a policy that cannot be enforced by
a compiler, type checker, linter, or commit gate is a strongly worded instruction, and
the census is what strongly worded instructions produce. The ban below is therefore
absolute (an exception class would be enforceable only as prose) and mechanized (D4).

Terminology, recorded in the GLOSSARY: a **retirement tripwire** is any test, gate, or
prose prohibition whose purpose is keeping a deleted thing gone. A **live-invariant
gate** guards a rule that present, living code could silently violate — the
no-pyo3-in-cores scan, the FFI-seam boundary (ADR-013), stub drift (ADR-066), the
push-gate manifest (ADR-074), the dependency charter (ADR-084). Live-invariant gates
are the GLOSSARY verification idioms; this ADR leaves them standing and gives each a
machine-readable label (D2). The two species differ in what they watch — the living
versus the dead — and conflating them is how the garrison grew.

## Decision

### D1 — Deletion is the complete retirement, with no exception class

When a cutover deletes a module, symbol, table, config key, or behavior, the deletion
finishes the retirement. Do not add a test, gate, or prose ban to keep the deleted
thing gone — for any surface, including the compiler-blind ones (the Python public
API, the FFI seam, external env/config/schema contracts, the commit-gate wiring). The
guards a retirement carries are the ones the artifact itself provides: the compiler and
type system for Rust-internal retirements; the owning ADR and `git log` for provenance;
review for everything else.

Two clarifications keep the absolute rule decidable:

- **Tests of present code are not tripwires.** A pointed retirement error (the
  `database upgrade` subcommand rendering its retirement line, a config loader refusing
  a retired key) is present code with present behavior; tests of that behavior are
  ordinary tests and stay. A test that asserts *absence* — `assert not hasattr` on a
  retired name, a grep for a retired identifier, "must not reappear" — is a tripwire
  and dies.
- **External consumers get loud errors, not guards.** When a retired contract may still
  be exercised from outside the repo (an old env var, a retired subcommand), the
  sanctioned answer is present code that fails loud at the boundary. The error path is
  the compatibility story; a guard watching for resurrection is not.

### D2 — Live-invariant gates are a different species, and the label is machine-readable

The gates ADR-047 D4 blessed ("source-scan tests are for LIVE invariants only") stay:
no-pyo3-in-cores, the FFI-seam boundary (ADR-013), stub drift (ADR-066), the push-gate
manifest (ADR-074), the dependency charter (ADR-084). They watch present code and are
among the strongest gates in the repo. Each carries an explicit live-invariant label
where it lives (doc comment or owning ADR), stating what present code it watches. The
label does double duty: it tells the next agent these are not precedent for tripwires,
and it is the exemption marker the enforcement gate (D4) recognizes. Unlabeled
tripwire-shaped artifacts fail the gate.

### D3 — ADR-047 D4 is reaffirmed in full

D4's principle was right — including its absolutism. What failed was its mechanism:
written nowhere agents look, and nothing enforced it. ADR-087 supplies both: the rule
in AGENTS.md (D1) and a commit gate (D4). ADR-047's status header records the
reaffirmation; its D1–D3 (the subscriber-bus retirement) are untouched.

### D4 — Enforcement is a commit gate: the tripwire ratchet

The ban is enforced mechanically. The tripwire ratchet is a source-scan gate wired into
the prek pre-push manifest and `just test`, scanning the test/gate surface (rust test
files, `tests/`, justfile, prek.toml, `scripts/`, `docs/`) for tripwire signatures —
the detection patterns the 2026-10-28 census used: absent-API assertions with
retirement vocabulary, retired-name greps, "must not reappear", and
"do not revive / re-land / rebuild" prose. Any hit fails the gate unless the artifact
carries the D2 live-invariant label. The ratchet:

- **Lands last**, after the cleanup epic's deletion batches, seeded on a tree with zero
  tripwires; from then on a new tripwire fails at commit/push time.
- **Is itself a live-invariant gate** — it watches the present tree for violations of
  a present rule — labeled per D2, with its red path demonstrated on a synthetic
  tripwire fixture before it is trusted (negative-probe idiom).
- **Owns no judgment calls.** It cannot decide whether an unlabeled artifact is a
  tripwire, and it does not try: the author either deletes the artifact or, if it is
  genuinely a live-invariant gate, labels it — which forces the D2 statement of what
  present code it watches. The decision is made visible, not automated.

### D5 — The accumulated census is triaged and cleaned

The 189 artifacts are triaged per-artifact, executed as an ergo epic:

- **Delete**: every retirement tripwire — the 11 prose bans (vocabulary-bearing ones
  fold into GLOSSARY `_Avoid_` lines, since the GLOSSARY owns vocabulary); orphan
  guards; ADR-owned guards, compiler-watched and compiler-blind alike. An owning ADR
  that named a guard gets a header note recording the deletion, breaking the link in
  both directions.
- **Keep**: live-invariant gates (relabeled per D2); tests of present-code behavior,
  including pointed-retirement-error paths (D1) — these were never tripwires.
- Each deletion lands in small batches with the full suite (`just test`) after each
  batch, never as one sweep: a tripwire that has been load-bearing in CI for months is
  infrastructure, and removing infrastructure needs the same evidence as adding it. A
  batch that turns red means the "guard" was hiding a live hole — fix the hole or
  reclassify with evidence; the tripwire does not come back.

## Considered options rejected

- **An exception class for compiler-blind surfaces** (admission criteria such as:
  owning ADR, demonstrated red path, decidable matching, a named kill condition, a
  recorded admission). Rejected: none of those criteria is enforceable by a compiler,
  linter, or commit gate — no mechanism can verify that a kill condition is sound or
  that matching is judgment-free — so the class would be gated by prose, and
  prose-gated exceptions are how the 189-artifact garrison grew. The blind-surface
  dangers are real (shadow authority on the Python surface, seam erosion at the FFI
  boundary, silent revival of external contracts, vacuous gate wiring); they are
  answered by loud boundary errors in present code (D1) and by review, not by a guard
  population that cannot be held to its own admission standard.
- **The ban with no enforcement gate.** Rejected: that is ADR-047 D4's exact mechanism,
  and the census is its result. Policy that must beat prior art is mechanized or it is
  a wish.
- **Keep everything; retroactively add ownership and red paths.** Rejected: it
  certifies the scar tissue instead of removing it, and certifying ~120 guards that
  never proved they fire is a heavier lift than triage, with nothing gained — the
  example problem the census exposed is the harm, and preservation does not fix it.
- **Date-based expiry for guards.** Rejected: dates fire whether or not the surface is
  stable — and under the absolute rule there are no guards left to expire.

## Consequences

- AGENTS.md carries the absolute rule; ADR-047's header records the reaffirmation; the
  README decisions table lists both; the GLOSSARY defines "retirement tripwire" as an
  `_Avoid_`-flagged term.
- The cleanup epic deletes every retirement tripwire in gate-verified batches; what
  survives is live-invariant gates (labeled) and ordinary tests of present behavior.
- The tripwire ratchet makes the ban self-sustaining: a new tripwire fails at
  commit/push time, and the exemption path (the D2 label) forces the author to state
  what present code the gate watches.
- "There is prior art" stops being an argument: surviving prior art is a labeled
  live-invariant gate or an ordinary behavior test, and both say so where they live.
- Future architecture reviews must not suggest retirement tripwires for any surface —
  carrying forward ADR-047's review instruction, now with teeth.
