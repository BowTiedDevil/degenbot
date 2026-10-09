# 2OWSYN — C1.3: the all-CL solve entry takes prepared hops; the table gate is deleted

Commit 3 of PR C1 (epic ZKHO3T, ADR-069). The all-CL entry now takes the
projection's prepared hops directly, so the pairing the table gate defended is
structural and the gate is gone.

## What changed

- `degenbot-solvers/src/cl/entries.rs`
  - `solve_cl_piecewise(prepared: &[PreparedClHop], memo, cfg, env)` — takes the
    prepared hops (the projection's `Arc`-shared sequence + crossing table +
    word profiles as ONE value) instead of the positional `sequences` +
    `prepared` pair. The memo key folds the hops' `int_seq` directly; hop
    assembly is `cl_walk_hop_cached(h.int_seq, &h.crossings, &h.profiles)`.
  - DELETED the per-hop table gate and `ClSolveTables::matches` (the per-hop
    `walk_path_fingerprint` re-derivation). The same gate in the mixed entry is
    gone too — `rg "matches\(|table gate"` on entries.rs is empty.
  - `derive_and_solve_cl_piecewise` is now derive-prepared-then-solve: it builds
    a `PreparedClHop` per sequence (`PreparedClHop::derive`) then calls the
    entry. The tableless-caller cost note stands (the caller pays derivation).
  - `ClSolveTables` SURVIVES: the mixed entry (`solve_mixed_piecewise`) still
    takes `&[Option<ClSolveTables>]`; the all-CL-only consumers are gone.
- Call sites migrated to `&[PreparedClHop]` (16 total):
  production `degenbot-substrate/src/resolve/mod.rs::all_cl_solve` and
  `degenbot-solvers/src/mixed/solve.rs` (all-CL branch, prepared-table
  construction moved into the mixed branch so all-CL paths no longer pay the
  translate); tests `cl/tests/mod.rs`, `cl/tests/memo.rs`,
  `cl/tests/refinements.rs`, `tests/cl_cache_lab_goldens.rs`; examples
  `walkmemo_readout.rs`, `cl_cache_lab.rs`; bench `walkmemo_contention.rs`;
  bot `arb_engine/walk_telemetry.rs` tests.
- Deleted the now-meaningless unit test
  `solve_cl_piecewise_refuses_prepared_tables_of_another_sequence` (it asserted
  the removed gate). The `ClPathSim` public helper and `ClSolveTables::derive`
  are left on `ClSolveTables` for C1.4 (the mixed side's consolidation).

Handful of doc comments updated to drop the gate's narrative.

## Gates (foreground, from `rust/`)

- `cargo check -p degenbot-solvers --all-targets` → clean (no warnings).
- `cargo check -p degenbot-substrate --all-targets` → clean.
- `cargo check -p degenbot-bot --all-targets` → clean.
- `cargo test -p degenbot-solvers` → all green (178 passed / 3 ignored lib +
  all integration suites; `v3_crossing_solver_vs_sim_parity` 4/4,
  `v4_crossing_solver_vs_sim_parity` 7/7, `v4_word_boundary_solver_divergence`
  2/2 all UNMODIFIED).
- `cargo test -p degenbot-substrate` → 355 passed / 0 failed.
- `cargo test -p degenbot-bot` → 714 passed / 3 ignored / 0 failed.

PENDING: none.

## Tree hygiene

Tree was clean at start. Files touched are exactly the 13 listed in the diff;
no foreign edits observed.
