# Comment hygiene

Rules for comments in code (Rust and Python). They exist because the
2026-09-15 tech-debt survey found the dominant failure modes: dead
task-tracker citations, gate-state narration, sequencing rules written
in prose, and acceptance criteria summarized as IDs.

## The first-home test

Every comment makes a claim. Put it in the FIRST home that can carry it,
and stop there:

1. **The code.** A name, type, enum, state machine, or function boundary.
   Temporal rules fit here: "call invalidate after any mutation" is a
   setter that does it; "must be called before the solve lane runs" is a
   phase type. If the comment can be made unenforceable-by-comment
   (compiled into the signature or a typestate), that is home 1.
2. **The comment.** The **why**: the invariant, the constraint imposed
   from outside the call site, the alternative that looks correct but
   is not, the external source of truth. Test: the comment survives a
   rename and still teaches something the signature cannot.
3. **The test.** Acceptance criteria live as a test name and assertion.
   A comment may point at the test (name it, or reference the fixture);
   it may never summarize the verdict ("per ergo DLSKD7 this must fire").
4. **The docs.** Ship history, measurements, decision rationale, and
   terminology live in `docs/adr/`, `docs/architecture/`, and
   `GLOSSARY.md` respectively — never in the code file's changelog voice.

## Banned in code

Anything that must be resolved outside the file AND is not permanent:

- **Task-system IDs** (ergo epic/task IDs, any 6-character tracker
  reference). Git commit messages carry them. Measured 2026-09-15:
  237 in-code citations, zero resolvable. Task IDs are "planning"
  artifacts, not "explanation" artifacts.
- **Migration work-item labels**: `ADR-005 slice 14j`, `Plan 102`, `slice-3b`.
  A slice is an ephemeral work item exactly like a tracker ID: the rule it
  introduced belongs in the comment, the sequencing history in the ADR. A
  sentence describing a then-future ("the pump lands in slice 5") that has
  since landed is stale on arrival — rewrite it in the present tense or delete
  it.
- **Gate narration**: "RED pin", "RED→GREEN", "post-fix", "until X
  merges". Code describes the present. Once a gate passes, the comment
  is sediment — delete it in the same change that proves it.
- **Provenance narration**: "split out of former monolith", "moved
  from py_binding.rs". `git log --follow` owns history.
- **Restating the code**: a comment the reader can skip without loss
  is deleted, not trimmed. If a comment's info became visible in the
  signature, the comment goes.

## Deleting with the change

When a change invalidates a comment's claim (fixes the bug it explains,
upgrades the sequencing it guards, closes the gap it describes), remove
or rewrite it in the same commit. Stale comments are defects, not
documentation debt; treat them like failing tests.

## Naming and language

Use the ubiquitous language (GLOSSARY.md); if you coin a term in a
comment, add it there instead. Docstrings on the Python public API are
not optional and are not the target of these rules — they follow their
existing conventions.

## Mechanical check

The enforced gate is `just lint-comment-hygiene`: one anchored detector with
zero tolerance, over the code-ish file classes in git's index. It catches
tracker citations (`ergo <ID>`, `ergo epic|task|slice <ID>`, a bare
`epic|task|slice <ID>`, RED/GREEN gate narration next to an ID),
migration-work-item labels (`slice 14j`, `slice-3b`, `Plan 102`, `plan-101`),
`TODO-<hex>` / `TODO(<word>)` tracker IDs, `Epic <token>` / `slice <token>` /
`Phase <letter><digit>` labels (the epic and slice shapes require a digit in
the token, so "an epic feature" and "the two-slice intake" are prose), the pre-
and post-epic forms, and the concrete incident labels a change leaves behind (a retired-artifact note, the
repair's name, a grep-compat marker, a crash ordinal, gate placeholders).
There is no census and no waiver: a red gate is repaired by editing the code.
See [gate ratchets](gates.md) for the shapes, the scope, and the entry recipe.

Two classes are **review-owned**, not gate-owned, because their surface forms
are indistinguishable from live prose:

- a **bare six-character token** with no anchor word. A citation, an acronym,
an EVM opcode and a scream-case static share one shape, and the lexicon that
told them apart needed a row per legitimate identifier — a file review had to
keep true forever.
- **broad past-voice narration**: a permanence assertion (`must STAY`, `the
guard STAYS`), the value a change replaced (`the prior constant`), slice-dated
deletion and then-future prose (`deleted in slice …`, `a later slice's …`),
the containing change's play-by-play (`this chunk's change`, `… surfaces
fixed`), the fix's history voice (`pre-fix`, `post-fix`, `before the fix`),
past-voiced removal (`was removed`), and the unambiguous past-habitual `used
to` verbs (`used to live/exist/die/own`). Present-tense emphasis (`is
UNCHANGED`, `the mode STAYS`), present passive mechanism (`is removed when`,
`lands its own spans later`) and the present-purpose `used to <verb>`
ellipses plus one runtime refusal string's `used to be` clause are live prose,
and they are why a lexical matcher cannot be trusted on this class.

Reviewers read for both. A swept site states its claim in the present ("the
failure this pins: …", "without the cache, …").

The optional grep below is a quick manual check for the worst offender
(tracker IDs):

```bash
git grep -nE '\berg(o)?[ :]*[A-Z0-9]{6}\b|-- .*\b[A-Z0-9]{6}\b' -- src rust/crates tests
```

Escape hatches: a TODO is allowed with an owner and a deadline, never a
tracker ID; binary-data hygiene ignores hex/base64 tokens that only
look like IDs.
