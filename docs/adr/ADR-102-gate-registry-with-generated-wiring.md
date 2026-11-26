# ADR-102: One registry for the gate tier, with generated wiring

**Status: accepted (implemented for the hook wiring; the doc table and the
parity manifest are pending, see Migration).** Elaborates
[ADR-074](ADR-074-push-gate-manifest.md)'s single-manifest principle and
[ADR-087](ADR-087-deletion-is-the-retirement.md)'s retirement
hygiene for the gate tier itself.

## Context

The repository carries 27 prek hooks, 37 `check-*`/`lint-*` recipes, nine
documented ratchets plus a 72-test architecture-gate family, and 15 CI jobs.
Each gate is declared in up to ten places: its body, its data file, a justfile
recipe, a prek hook, a `docs/gates.md` table row, its section, a negative-probe
suite, a `ci.yml` job, a `scripts/hooks/ci-parity-manifest.txt` entry, and a
`scripts/hooks/ci-recipe-tools.txt` row.

Measured consequences in one week of work:

- Retiring the `too_many_lines` census was a ten-file change for one gate
  (−1075 lines, +20).
- Structural edits forced test edits: two suites pinned the *text* of the
  wiring (the exact `lint-rust-check:` dependency line, the hook's stage list),
  so any re-wiring broke a test that asserted prose.
- A gate whose data file was tracked but whose subjects were untracked failed
  pushes from a different lane, invisibly to the commit's author.
- 28% of the last 200 commits touch a bookkeeping file, and the three
  highest-churn surfaces (tool catalog 53, capability catalog 24, dependency
  charter 2) have no recorded defect (ADR-102's audit, `docs/gates.md`).

The repeated cost is not any single gate; it is that a gate's identity is spread
across files that must agree by hand.

## Decision

**D1 — one declaration per gate.** `scripts/gates.toml` is the single source of
a gate's identity: `id`, `tier` (prek stage or CI job), `scope` (`index`,
`staged`, or `tree`), `command`, `data` paths, the `docs/gates.md` anchor it
owns, and its probe suite. Prose rationale stays in `docs/gates.md` and the
body's header comment; the registry carries the facts that must agree.

**D2 — generated wiring.** `just gates-regen` renders the registry into the
wiring; hand-editing a generated region fails `just gates-check`, which
regenerates and diffs (the idiom the stub and census gates already use). In the
shipped phase it renders `prek.toml`'s hook *lines*, leaving the rationale prose
in `prek.toml` beside the gate it explains — a hook's facts are generated, its
argument stays where a reader looks for it. The `docs/gates.md` table rows and
the `scripts/hooks/ci-parity-manifest.txt` entries follow in phase 3.

**D3 — the registry is the retirement surface.** Deleting a gate is deleting its
entry and its body; the generator removes its wiring in the same commit. No
waiver file, no parked name, no "do not revive" prose (ADR-087).

**D4 — a gate lands with three things.** Its registry entry, a `fix:` line on its
red path, and — for a ratchet — a recorded catch in the `docs/gates.md` table
(ADR-102's audit rule: documentation consistency survives only while it keeps its
document true).

**D5 — generated data where derivable.** A gate whose census is a projection of
the tree gains a `--rows`/`--write` form and its rows are generated, so a landing
change runs a command instead of hand-editing a table. The three unrecorded,
high-churn ratchets are the first candidates.

## Consequences

- Adding or retiring a gate becomes one edit plus its body; the ten-place edit
  disappears, and with it the class of failure where two declarations disagree.
- `prek.toml`'s per-hook rationale comments move into `docs/gates.md` sections
  the registry anchors to; the generated hook entries are terse. This is a real
  loss of locality and is accepted to buy a single source.
- The generator must be able to reproduce today's wiring exactly, so the
  migration is verifiable by "regenerate, diff, expect empty" on every phase —
  no gate changes behaviour during the move.
- Wiring-text tests are deleted rather than updated (they were the symptom of
  hand-maintained wiring); behaviour is asserted by running the recipe.

## Alternatives considered

- **Keep hand-maintained wiring, add more parity tests** — rejected: that is the
  status quo that produced the ten-file edit and the two prose-pinning suites.
- **One monolithic `just verify`** — rejected: loses per-gate identity, failure
  attribution and the ability to run one gate.
- **Generate `prek.toml` wholesale from the registry** — rejected: the file's
  per-hook prose is genuine rationale (scope choices, latency budgets) that a
  generator would flatten into field names.

## Migration

1. ~~Registry + `gates-regen` + drift gate over the **pre-push** hooks, verified
   by an empty regeneration diff.~~ Done: all 21 hooks (pre-commit, commit-msg and
   pre-push) are declared in `scripts/gates.toml`; the first regeneration changed
   four lines of field order and nothing else, and the hook set is identical as
   data. `tests/test_gate_registry.py` carries the drift assertion and the
   rejection cases.
2. ~~Extend to the **pre-commit** hooks.~~ Done with phase 1: the registry is one
   list, so the stages came with it.
3. Extend to the `docs/gates.md` table rows and the `ci-parity` manifest.
4. ~~Convert the three unrecorded high-churn censuses to `--rows` generation
   (D5).~~ Measured and declined: the churn on those rows is content, not
   bookkeeping (the last six tool-catalog commits changed one to three lines
   each, all substantive). D5 stays a principle for data that is *purely*
   derived, and no such surface is currently a census.
