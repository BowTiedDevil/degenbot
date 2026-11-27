# ADR-102: One registry for the gate tier, with generated wiring

**Status: accepted / implemented** (hook wiring, identity facts, CI
coverage validation, and the ci-parity manifest split; the `docs/gates.md`
ratchet tables remain authored, see Migration). Elaborates
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
a gate's identity: `id`, `stage` (the prek stage; the `tier` name in the
reviewed plan), `entry` (the command), `scope` (`index`, `staged`, or
`tree` — the population judged; `index` iff prek hands the staged file list),
`data` paths, the `docs/gates.md` heading that documents it, its `probe`
pytest node ids, and exactly one CI coverage form. Prose rationale stays in
`docs/gates.md` and the body's header comment; the registry carries the facts
that must agree.

**D1b — one CI coverage form per hook, and no local-only waiver.** Every
mandatory hook declares one of four forms, each validated by `gates-check`
against the live workflow and justfile:

- `ci_job` + `ci_entry` + `ci_relation = "direct"` — the job runs the hook's
  entry verbatim (a script path or `just <recipe>`);
- `ci_entry` + `ci_relation = "contains"` — the job runs `ci_entry`, and the
  `just --dump` invocation graph (declared dependencies plus `just <recipe>`
  calls in the body, arguments included) reaches the hook's entry recipe;
- `ci_entry` + `ci_relation = "equivalent"` + `ci_equivalent_probe` — the job
  runs a parallel whole-tree/range implementation and the named test pins the
  equivalence;
- `ci_probe` — a committed-tree pytest node is the CI execution, validated
  statically (path plus import-free `ast` function presence, never pytest
  collection in a commit hook) and, on the pytest side, by a default-suite
  eligibility check.

Presence in `ci.yml` is never sufficient: `direct` requires the entry
verbatim, `contains` requires reachability, and `equivalent` requires its
probe. The registry also validates each gate's `data` paths, its `docs`
heading and its probe node ids, so a gate cannot claim a surface the tree has
lost.

**D2 — generated wiring.** `just gates-regen` renders the registry into the
wiring; hand-editing a generated region fails `just gates-check`, which
regenerates and diffs (the idiom the stub and census gates already use). In the
shipped phase it renders `prek.toml`'s hook *lines*, leaving the rationale prose
in `prek.toml` beside the gate it explains — a hook's facts are generated, its
argument stays where a reader looks for it. Prose is owned structurally: the
comment block between the previous hook line and its own belongs to that gate,
while a stage's `# --- <stage> ---` separator and the block under it anchor the
stage, so the first hook of a stage can be retired without deleting the section
header. Check and regen differ in exactly one case, on purpose: a hook line the
registry does not declare is drift `check` refuses and `regen` retires, and
prose neither a hook nor a stage anchor owns fails both rather than being
guessed at.

``ci-parity`` is split rather than duplicated: the registry validates a hook's
own CI coverage, and `scripts/hooks/ci-parity-manifest.txt` (`job|kind|notes`)
keeps every `ci.yml` job accounted for — the hook-less jobs and the jobs whose
local reproduction spans several recipes — plus the recipe toolchains those jobs
need. The manifest's retired `push_gates` column was a string-level copy of the
per-hook fact the registry now proves. The `docs/gates.md` *ratchet tables*
(the censuses' recorded catches and repair rows) remain authored: they carry
judgment, not derivable facts.

**D3 — the registry is the retirement surface.** Deleting a gate is deleting its
entry and its body; the generator removes its wiring in the same commit. The
hook line and the prose it owned go together, while the hand-written stage intro
survives, because it explains the stage rather than the retired hook. No waiver
file, no parked name, no "do not revive" prose (ADR-087).

**D4 — a gate lands with four things.** Its registry entry (identity, scope,
data, docs, probe and CI coverage), its `fix:` line on the red path, its probe
test naming the defect class it catches, and — for a ratchet — a recorded catch
in the `docs/gates.md` table (ADR-102's audit rule: documentation consistency
survives only while it keeps its document true).

**D5 — generated data where derivable.** A gate whose census is a projection of
the tree gains a `--rows`/`--write` form and its rows are generated, so a landing
change runs a command instead of hand-editing a table. The three unrecorded,
high-churn ratchets are the first candidates.

## Consequences

- Adding or retiring a gate becomes one edit plus its body; the ten-place edit
  disappears, and with it the class of failure where two declarations disagree.
- The registry validates more than wiring: a scope/file-list mismatch, a missing
  data path, a docs heading or a probe test, and a CI coverage claim the workflow
  or the `just` invocation graph does not support all fail `just gates-check`
  (`scripts/gate_registry.py`), so a gate cannot outlive its evidence.
- The CI coverage facts are checked without running the tests they name: the
  commit hook parses the probe files with `ast`; only the pytest suite asserts
  that a `ci_probe` is inside the default selection.
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
   by an empty regeneration diff.~~ Done: all 22 hooks (pre-commit, commit-msg and
   pre-push) are declared in `scripts/gates.toml`; the first regeneration changed
   four lines of field order and nothing else, and the hook set is identical as
   data. `tests/test_gate_registry.py` carries the drift assertion, the
   rejection cases, the retirement asymmetry and the stage-anchor cases.
2. ~~Extend to the **pre-commit** hooks.~~ Done with phase 1: the registry is one
   list, so the stages came with it.
3. ~~Extend to the `ci-parity` manifest.~~ Done: the `push_gates` column is
   retired, the manifest is `job|kind|notes` and keeps job completeness plus
   toolchain provisioning, and each hook's CI coverage is a validated registry
   field (`ci_job`/`ci_entry`/`ci_relation`/`ci_equivalent_probe`, or
   `ci_probe`). The `docs/gates.md` *ratchet tables* stay authored: their rows
   carry recorded catches and repairs, which are judgment rather than derivable
   facts.
4. ~~Convert the three unrecorded high-churn censuses to `--rows` generation
   (D5).~~ Measured and declined: the churn on those rows is content, not
   bookkeeping (the last six tool-catalog commits changed one to three lines
   each, all substantive). D5 stays a principle for data that is *purely*
   derived, and no such surface is currently a census.
