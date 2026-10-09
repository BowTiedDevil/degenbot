# KUFFB7 — P1: delete the nearly-dead fingerprint field on PreparedClHop

STATUS: **DONE.** Both scoped gates are green on the current tree; nothing PENDING.

The stored per-hop fingerprint on `PreparedClHop` fed the per-hop table gate that
C1.3 deleted (ADR-069 D4). Post-C1 nothing in production reads it: the all-CL memo
key already folds the hops' sequences directly, and the mixed entry folds the views.
The field, its `fingerprint()` accessor, and the two `walk_path_fingerprint` folds
in `derive` / `from_parts` are gone; the one test reader now folds directly.

## What changed

- `rust/crates/engine/degenbot-solvers/src/cl/entries.rs`
  - Dropped the `fingerprint: u128` field and its doc comment from `PreparedClHop`.
  - Dropped the `pub const fn fingerprint(&self) -> u128` accessor.
  - Removed the `let fingerprint = walk_path_fingerprint(&[int_seq.as_ref()]);`
    fold from both `derive` and `from_parts`; the struct literals now carry only
    `int_seq` / `crossings` / `profiles`.
  - Rewrote the value's doc comment: its point is now the STRUCTURAL pairing
    (sequence + tables travel as one value), not the fingerprint. The `derive`
    and `from_parts` docs no longer claim a fold.
  - `walk_path_fingerprint` stays imported and used: `solve_cl_piecewise` still
    folds the hops' sequences for the memo-probe key (`entries.rs:167`).
- `rust/crates/engine/degenbot-solvers/src/cl/memo.rs` (test only)
  - The one reader (`prepared_key`, was `:544`) now calls `walk_path_fingerprint(&[seq])`
    directly instead of `PreparedClHop::derive(...).fingerprint()`.
  - Removed the now-unused `use crate::cl::PreparedClHop;` import; reworded the
    two comments around the mutation sweep. The sweep itself is unchanged.
- `docs/adr/ADR-069-prepared-hop-entry-surface.md`
  - One-line Consequences addendum: the stored per-hop fingerprint retired with the
    table gate it fed; D5's fold body stays byte-identical and crate-private.

## Verification

`rg "fingerprint()" --type rust rust/crates` and `rg "source_fingerprint" --type rust
rust/crates`: no remaining hits on `PreparedClHop`. The residual `fingerprint()` hits
are unrelated build-receipt code (`degenbot-python` / `degenbot-cli` build identity).

## Gates (foreground)

- `cargo test -p degenbot-solvers` — all suites green (177 + 20 + 7 + 7 + 4 + 3 +
  2 + 1 + 1 + 1 + 1 passed, 0 failed, 3 ignored; doc-tests 0).
- `cargo clippy -p degenbot-solvers --all-targets` — clean, no warnings
  (rustc/cargo 1.98.1, the pinned toolchain).
- `cargo fmt -p degenbot-solvers` — no diff produced.

## Residual (NOT changed — outside P1's named scope)

Two doc comments still describe the deleted field. Flagged for the manager, not
edited, because P1's declared file set was entries.rs + memo.rs tests + ADR-069:

- `rust/crates/engine/degenbot-solvers/src/cl_cache.rs:51-53` — "folding the
  sequence's derivation fingerprint once so every strategy hands back the same
  `PreparedClHop` shape".
- `rust/crates/engine/degenbot-solvers/src/mixed/mod.rs:334-337` and `:426-427` —
  `ResolvedHop::V3/V4` and `as_prepared_cl_hop` docs list "the sequence's derivation
  fingerprint" as part of the prepared value.
