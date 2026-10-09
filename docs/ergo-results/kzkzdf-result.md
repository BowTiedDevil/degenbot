# KZKZDF — C1.4: the mixed solve entry takes one hop-view slice; the per-solve rebuild is deleted

Commit 4 of PR C1 (epic ZKHO3T, ADR-069). The mixed entry now takes ONE slice
of the borrowed hop view `V2(&IntHopState) | Cl(&PreparedClHop)`, the parallel
positional slices + `hop_order` bools are gone, and the per-solve
`ClSolveTables` rebuild inside `mixed/solve.rs` is deleted. `ClSolveTables` is
deleted outright (nothing references it).

## What changed

- `degenbot-solvers/src/cl/entries.rs`
  - `solve_mixed_piecewise(hops: &[MixedHopView<'_>], memo, cfg, env)` — takes
    ONE borrowed hop view per position instead of
    `(v2_hops, cl_sequences, cl_prepared, hop_order)`. The invariants the
    parallel slices could not carry (parallel lengths; which slice carries a
    `Some`) are now structural: the family tag and the prepared value ride the
    same slice entry, and the only remaining gate is `hops.len() < 2`.
  - NEW `MixedHopView<'a> { V2(&'a IntHopState), Cl(&'a PreparedClHop) }` with a
    private `piece_view()` that is the whole hop-assembly seam: V2 borrows the
    state; CL O(1)-clones the projection's `Arc` tables via
    `cl_walk_hop_cached` (no re-derivation, no rebuild).
  - The mixed memo probe still keys by `walk_mixed_path_fingerprint` (signature
    unchanged) — the key is re-projected from the view (a V2 `IntHopState`
    clone for the fold only, never for the walk), so it is byte-identical to
    the pre-change key.
  - `ClSolveTables` and `PreparedClHop::to_solve_tables` DELETED; the old
    `cl_hop_view(seq, Option<&ClSolveTables>)` seam deleted.
- `degenbot-solvers/src/mixed/solve.rs` (`solve_walkable_path_int`): the
  `hop_order`/`v2_hops`/`int_v3_sequences`/`cl_prepared` flattening (and the
  `to_solve_tables` translate) is replaced by ONE borrowed
  `Vec<MixedHopView<'_>>` built by matching `ResolvedHop` variants. `all_cl` is
  recomputed structurally. A non-V2/CL family refuses (`None`) rather than
  solving a partial composition.
- `degenbot-solvers/src/cl/path_sim.rs`: `ClPathSim::new(sequences, prepared)`
  now takes `Option<&[PreparedClHop]>` (was `Option<&[ClSolveTables]>`);
  `ClPathSim` itself is unchanged (ADR-069 out-of-scope note stands).
- `degenbot-solvers/src/cl/crossings.rs`: `cl_walk_hop` (the cache-less
  builder) is now `#[cfg(test)]` — its only remaining non-test caller was the
  deleted `cl_hop_view` `None` arm; tests keep using it.
- `degenbot-solvers/src/cl/mod.rs`: re-export `MixedHopView` in place of
  `ClSolveTables`; module-layout doc updated.
- `degenbot-solvers/src/cl/memo.rs`: doc-comment rename
  (`ClSolveTables::source_fingerprint` → `PreparedClHop::source_fingerprint`).
- `foundation/degenbot-substrate/src/resolve/mod.rs`: `mixed_solve` builds one
  borrowed hop view and calls the new entry.
- Test/example call sites migrated (mechanically): `cl/tests/mod.rs` (5 tests),
  `cl/tests/memo.rs` (4 mixed memo tests — call sites only; every assertion is
  byte-for-byte untouched), `cl/tests/refinements.rs` (deleted the
  `prep_as_options` translator; the Loop-21 mixed test now passes a view),
  `tests/mobius_discriminant_domain.rs` (all-V2 grid), and
  `examples/mixed_solve_replay.rs` (derives prepared hops once per path, then
  times the solve).

## Gates (foreground, from `rust/`)

- `cargo test -p degenbot-solvers` → all green (lib 178 passed / 3 ignored, same
  count as C1.3; `v3_crossing_solver_vs_sim_parity` 4/4,
  `v4_crossing_solver_vs_sim_parity` 7/7, `v4_word_boundary_solver_divergence`
  2/2, `cl_cache_lab_goldens` 1/1, `mobius_discriminant_domain` 3/3 — all
  UNMODIFIED).
- `cargo test -p degenbot-substrate` → 355 passed / 0 failed.
- `cargo test -p degenbot-bot` → 714 passed / 3 ignored / 0 failed.
- `cargo check -p {degenbot-solvers,degenbot-substrate,degenbot-bot} --all-targets`
  → clean.
- `cargo clippy -p {degenbot-solvers,degenbot-substrate,degenbot-bot} --all-targets`
  → clean once the PRE-EXISTING `profit_envelope.rs:1214,2132`
  `semicolon_if_nothing_returned` findings (untouched file, newer clippy) are
  allowed. Not introduced by this chunk.
- `cargo fmt -p degenbot-solvers -p degenbot-substrate -- --check` → clean.
- `rg "ClSolveTables" --type rust rust/crates` → zero hits.

PENDING: none.

## Tree hygiene

Tree was clean at start. Files touched are exactly the 12 in `git diff --stat`;
no foreign edits observed. The `profit_envelope.rs` clippy findings above are
pre-existing in a file this chunk did not touch.
