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
   `CONTEXT.md` respectively — never in the code file's changelog voice.

## Banned in code

Anything that must be resolved outside the file AND is not permanent:

- **Task-system IDs** (ergo epic/task IDs, any 6-character tracker
  reference). Git commit messages carry them. Measured 2026-09-15:
  237 in-code citations, zero resolvable. Task IDs are "planning"
  artifacts, not "explanation" artifacts.
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

Use the ubiquitous language (CONTEXT.md); if you coin a term in a
comment, add it there instead. Docstrings on the Python public API are
not optional and are not the target of these rules — they follow their
existing conventions.

## Mechanical check

Optional grep gate for the worst offender (tracker IDs):

```bash
git grep -nE '\berg(o)?[ :]*[A-Z0-9]{6}\b|-- .*\b[A-Z0-9]{6}\b' -- src rust/crates tests
```

Escape hatches: a TODO is allowed with an owner and a deadline, never a
tracker ID; binary-data hygiene ignores hex/base64 tokens that only
look like IDs.
