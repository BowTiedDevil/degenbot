IMBX7I — docs/gates.md: index the ratchet family (censuses, gate bodies, entry recipes)

## What changed (docs only; no production code)

- docs/gates.md (NEW): indexes the five ratchets + the architecture_gates.rs family.
  Per ratchet: failure class (with incident), gate body, census/data file, how to add an
  entry (exact command or file edit), and how to run it locally; plus a table, a
  "The architecture_gates.rs family" section listing all 20 tests, and an
  "Adding a new ratchet" section naming the two idioms (script+lexicon; cargo
  architecture-gate test+census file).
- docs/comments.md: the "Mechanical check" section now names the enforced gate
  (`just lint-comment-hygiene`, both detectors) and cross-links [gate ratchets](gates.md).
- scripts/hooks/comment-hygiene-census.txt: header line "Index: docs/gates.md §1".
- rust/crates/facade/degenbot/tests/config_derived_statics_census.txt: header line
  "Index: docs/gates.md §2".

## Gates run (scoped, all foreground)

- just lint-comment-hygiene -> EXIT=0 (re-run after the census-header edit; still 0).
- just check-config-derived-statics -> EXIT=0 ("config_derived_statics_are_classified ... ok";
  re-run after the census-header edit; still 0).
- cargo test --locked --manifest-path rust/Cargo.toml -p degenbot-core worker_census
  -> 4 passed (register_upserts..., boot_table_emits..., export_hook_fires..., comm_histogram_census_matches_registry).
- just gen-stubs --check -> EXIT=0, "committed stubs match generator output" (27 files).
- just test-python-noise-gate '' tests/test_no_sqlalchemy_test_imports.py -> recipe ran end-to-end,
  "clean capture (suite exit 0)" EXIT=0. Negative probe: bash scripts/pytest_noise_gate.sh scan
  /tmp/spray.log (with a "Reconnection attempt" line) -> EXIT=3, prints the offending line;
  clean capture -> EXIT=0.
- Markdown gate: npx --yes markdownlint-cli2 docs/gates.md docs/comments.md -> 0 issues, EXIT=0
  (the repo's gate is `just lint-markdown`; the pre-commit `markdownlint` hook runs the same
  invocation check-only over the staged list).

## PENDING

- The full-suite form `just test-python-noise-gate` (no path arg) was NOT run: the ~3100-test
  suite exceeds the harness bound. The recipe itself was verified with a scoped target (above),
  and the script's run/scan/exit-code contract was exercised via the negative + clean probes.
  The manager re-runs the full suite at review.

## Foreign tree activity

- None observed. `git status --porcelain` at the end shows only my four paths
  (docs/gates.md untracked; docs/comments.md, the two census headers modified). Tree left dirty;
  no commit.
