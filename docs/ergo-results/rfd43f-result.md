# RFD43F — C1.5: the fingerprint goes crate-private behind the value; the golden pins become a mutation sweep; the last intake twins are deleted

Commit 5 of PR C1 (epic ZKHO3T, ADR-069). The composition key now derives from
the same prepared value the walk consumes: the fingerprint functions are
crate-private, `PreparedClHop` carries and exposes the value folded once at
derivation, the golden-pin/verbatim-copy tests are replaced by a mutation
sweep, and the remaining intake twins (`PreparedHop`, `ClPathSimHop`) are
deleted.

## What changed

- `degenbot-solvers/src/cl/memo.rs`
  - `walk_path_fingerprint` / `walk_mixed_path_fingerprint` are now
    `pub(crate)` (were `pub`), dropped from the `cl` public re-export.
  - The `#[cfg(test)] mod tests` that held the frozen
    `walk_path_fingerprint_original` twin, the five bit-identity tests, and the
    golden `u128` pin (with its STABILITY CONTRACT comment) is REPLACED by a
    **mutation sweep** (see below). The fold body itself is byte-identical —
    no fold line, constant, or mixing order changed.
  - `fold_sequence_ranges` doc no longer points at the deleted verbatim twin.
- `degenbot-solvers/src/cl/entries.rs`
  - `PreparedClHop.source_fingerprint` (pub field) becomes the private
    `fingerprint` field; `PreparedClHop::fingerprint() -> u128` is the accessor
    (`#[must_use] const fn`). The value is still folded ONCE at derivation in
    `derive` / `from_parts`.
- `degenbot-solvers/src/cl/tests/memo.rs`: the memo behavior tests are
  UNMODIFIED (hit/negative/miss, census eviction, reorg hold; they import the
  now-`pub(crate)` fns directly from `crate::cl::memo`).
- `degenbot-solvers/src/cl_cache.rs`
  - `pub type PreparedHop = (Arc<ClCrossingTable>, Arc<ClProfileTable>)` DELETED.
  - `ClCacheStrategy::refill` now returns `Vec<PreparedClHop>`; a private
    `prepared_hop(seq, crossings, profiles)` helper wraps the built tables as
    the one intake value. All eight strategies (S0–S7) and `fresh_tables`
    migrate; cached entries now store the prepared value and clone it (the
    `Arc` tables are unchanged — the lab's `Arc::as_ptr` reuse assertion still
    holds via `prepared[0].crossings`). No table build/reuse logic changed.
- `degenbot-solvers/src/cl/path_sim.rs`: the private `ClPathSimHop { crossings,
  profiles }` twin DELETED — `ClPathSim` stores `Vec<PreparedClHop>` (built with
  `from_parts` when no prepared hop is supplied). `ClPathSim` itself stays
  (ADR-069 out-of-scope note).
- `degenbot-solvers/examples/cl_cache_lab.rs`,
  `tests/cl_cache_lab_goldens.rs`, `tests/cl_cache_patch_regressions.rs`: the
  per-solve `PreparedHop` → `PreparedClHop::from_parts` re-pairing is deleted;
  the strategies hand back the prepared value and the call sites solve it
  directly (`prepared[0].crossings`).

## The mutation sweep (replaces the golden pins)

`src/cl/memo.rs::tests` now perturbs one `PieceView`-consumed field at a time
and asserts the key moves:

- CL (via `PreparedClHop::derive(seq).fingerprint()`): `liquidity`,
  `sqrt_price_x96`, `sqrt_price_lower_x96`, `sqrt_price_upper_x96`,
  `gamma_numer`, `fee_denom`, `zero_for_one`, `word_boundary_prices` (drop one,
  and add one to range 0's listing).
- CL path structure: hop order, position index, duplicate-vs-distinct pair,
  and the empty-sequence tag.
- V2 (via `walk_mixed_path_fingerprint`): `reserve_in`, `reserve_out`,
  `gamma_numer`, `fee_denom`.
- Mixed structure: the V2/CL family tag and the absent-position tag.

The field list is enumerated from `src/cl/active_set.rs` (`PieceView` /
`PieceFamily` and the `IntHopState` fields it reads).

## Negative probe (the gate is live, not theater)

Temporarily commented out one fold line in `fold_sequence_ranges`
(`mix_u256(lane_a, U256::from(r.gamma_numer));`), ran
`cargo test -p degenbot-solvers --lib fingerprint_moves_for_every_piece_view_cl_field`:

```
test cl::memo::tests::fingerprint_moves_for_every_piece_view_cl_field ... FAILED
assertion `left != right` failed: fingerprint did not move when
PieceView-consumed CL field `gamma_numer` changed
test result: FAILED. 0 passed; 1 failed
```

Restored from the pre-probe copy (`diff` → IDENTICAL, no `PROBE-DISABLED`
marker remains) and re-ran the four sweep tests → all green. The sweep fails
when a consumed field stops moving the key.

## Gates (foreground, from `rust/`)

- `cargo test -p degenbot-solvers` → green. lib `176 passed / 0 failed / 3
  ignored`; `cl_cache_lab_goldens` 1/1, `cl_cache_patch_regressions` 3/3,
  `mobius_discriminant_domain` 3/3, `v3_crossing_solver_vs_sim_parity` 4/4,
  `v4_crossing_solver_vs_sim_parity` 7/7, `v3_iia_fixture_reproduction` 1/1,
  `profit_envelope_tests` 20/20, `bounded_brent` 7/7, `offline_heavy_replay`
  1/1, `v4_word_boundary_solver_divergence` 2/2.
- `cargo clippy -p degenbot-solvers --all-targets` → CLEAN (exit 0, no
  warnings) under the pinned 1.98.1 toolchain.
- `cargo fmt -p degenbot-solvers -- --check` → clean.
- `cargo check -p degenbot-substrate -p degenbot-bot` → clean (the removed
  public re-exports and the now-private `fingerprint` field break no downstream
  crate; `degenbot-substrate/src/resolve/mod.rs` was NOT touched, so its test
  suite was not run).
- `rg "PreparedHop" --type rust rust/crates` → zero hits.
- `rg "source_fingerprint" --type rust rust/crates` → zero hits (renamed;
  docs outside `rust/` still mention the historical name).

PENDING: none.

## Tree hygiene

Tree was clean at start (`git rev-parse HEAD` = 5cd93497d). Files touched are
exactly the 8 in `git diff --stat`; no foreign edits observed.
