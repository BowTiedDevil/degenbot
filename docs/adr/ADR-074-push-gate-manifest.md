# ADR-074: The push-gate manifest — `just pre-push` delegates to prek.toml's pre-push entries

**Status: accepted / implemented** (drafted at the push-gate-manifest grilling,
2026-10-16; landed as a hard cutover in one commit).

> **Header note (ADR-087 D5).** The census gate this ADR named was deleted under ADR-087 D1/D5 (the absence half of `test_the_manifest_is_a_file_the_script_reads`) — the link breaks in both directions.

## Context

`just pre-push` was a hand-written 21-row `run_gate` transcription of the
`stages = ["pre-push"]` entries in prek.toml. A transcription is an artifact
that drifts, and this one did: the recipe ran four gates short and pointed two
of them at a different entrypoint than the installed hook, so `just pre-push`
reported green while `git push` was blocked by a gate the recipe never ran. The
drift direction is the bad one — AGENTS.md points every agent at `just pre-push`
before pushing, so a stale transcription is a false green that keeps producing
the miss.

Three more artifacts existed only to police the copies: the pre-push gate drift
brake (`scripts/hooks/pre-push-gate-drift.sh`, comparing the two lists), a
negative-probe suite over that brake, and ci-parity's marker-based wiring
check, which grepped `run_gate <name>` lines in the justfile. The brake itself
carried a fourth copy — the sed/awk extraction that re-derived each list.

## Decision

**D1 — One owner.** prek.toml's `stages = ["pre-push"]` entries ARE the
push-gate manifest (GLOSSARY: new term). The gate list is declared once; there
is no second list to keep in sync and no third artifact to compare them.

**D2 — `just pre-push` delegates.** The recipe is an install guard plus
`prek run --stage pre-push`, and owns no gate list. prek's native per-hook
output (the `name` fields) replaces the `run_gate` banners, and each hook's
`fail_fast = true` carries the fail-fast property the recipe used to hand-roll.

**D3 — Generation is rejected.** No generator emits the recipe from prek.toml
(or prek.toml from the recipe). A generator is a build step whose output must be
committed and drift-checked — the same problem with more machinery — while prek
is already the runner the hook stage uses. The recipe can simply call it.

**D4 — The drift machinery is deleted.** The brake script, its negative-probe
suite, and ci-parity's `run_gate` grep are removed outright, and the
`pre-push-gate-drift` entry leaves prek.toml. Nothing remains to compare, so a
probe of the one-line recipe would be a vacuous green.

**D5 — ci-parity's wiring check is retargeted.** The manifest's `push_gates`
column names entry strings that must appear among prek.toml's
`stages = ["pre-push"]` entries, replacing the note-embedded marker that was
grepped against justfile `run_gate` lines. The manifest itself is promoted from
a heredoc inside `scripts/hooks/ci-parity.sh` to the first-class data file
`scripts/hooks/ci-parity-manifest.txt` (`job|kind|push_gates|notes` pipe
format), and the gate gains the negative-probe suite it never had
(`tests/test_ci_parity.py`).

_live-invariant gate (ADR-087 D2): watches the live `push_gates` entry strings in `scripts/hooks/ci-parity-manifest.txt` and the prek.toml `stages = ["pre-push"]` entries they must name; not tripwire precedent._

**D6 — Hard cutover.** No flag, no transition period, one commit. The recipe is
a leaf nothing else calls, and CI invokes individual recipes rather than
`just pre-push`.

## Consequences

- The "shorter list reports green" failure mode is gone structurally rather than
  detected: `just pre-push` and an actual `git push` now run the same command
  (`prek run --stage pre-push`), so they cannot disagree about which gates ran.
- Adding, removing, renaming, or re-pointing a gate is a one-place edit in
  prek.toml.
- `just pre-push` inherits prek's own guards: prek must be on PATH (a global uv
  tool, installed by `just setup-git-hooks` and on demand by the recipe), and
  prek refuses to run while its config file has unstaged changes — the same
  refusal the installed hook produces, which is the point of the delegation.
- The pre-push `entry` strings must stay on one line: ci-parity extracts them
  with a sed anchored on the single-line `{ id = ... }` table opener, so a
  multi-line TOML hook table would be invisible to that check.
- The stage is now the only declaration of the gate list, so any gate that must
  be observed elsewhere (a CI job's local entrypoint, a coverage claim) names
  its `just` entry string from this list.

## Related

- **ADR-072** (the CI-only failure has a reproducer) — the brake that gives
  every CI job a named local entrypoint; ci-parity is that brake, and its
  wiring check is what this ADR retargets.
