AC4IBB (C1.2): the PreparedClHop value, constructed by the projection, carried in ResolvedHop.

Introduced `PreparedClHop` in `degenbot-solvers/src/cl/entries.rs` (beside the retired-next-chunks
`ClSolveTables`): the tick-range sequence (`Arc<IntV3TickRangeSequence>`) paired with its `Arc`'d
crossing table (`Arc<ClCrossingTable>`), word profiles (`Arc<ClProfileTable>`), and the sequence's
derivation fingerprint (`u128`, `walk_path_fingerprint(&[seq])`). `PreparedClHop::derive` folds the
crossing build + word-profile derivation + fingerprint ONCE (the fused S1 derivation);
`from_parts` wraps pre-built tables (offline/replay callers); `to_solve_tables` translates to the
entries' unchanged `ClSolveTables` intake.

`ResolvedHop::V3`/`V4` now carry `prepared: PreparedClHop` instead of the loose
`int_seq` + `word_profiles` + `crossing_table` triple. The existing accessors (`as_int_sequence`,
`as_word_profiles`, `as_crossing_table`) read from the value; a new `as_prepared_cl_hop` returns it.

The projection constructs the value: `degenbot-substrate/src/resolve/cl.rs` (`project_v3`,
`project_v4`) calls `PreparedClHop::derive(int_seq)` — the `fused_cl_tables` helper is gone, its
table-derivation guardrail folded into the module doc. The solve-time fingerprint fold in
`mixed/solve.rs` moved to derivation time: the solve now translates the carried value via
`as_prepared_cl_hop()?.to_solve_tables()`; `walk_path_fingerprint` is no longer called in the
solve entry assembly. The fold function itself, the entries, the table gate, and the memo are
untouched.

Byte-identical by construction: the crossing table / word profiles are the same `Arc`s the
projection built before, and `source_fingerprint` equals `walk_path_fingerprint(&[seq])` for the
same sequence — only the call site moved from solve time to derivation time.

Files changed:
- `degenbot-solvers/src/cl/entries.rs` — the `PreparedClHop` value + constructors/translate.
- `degenbot-solvers/src/cl/mod.rs` — export `PreparedClHop`.
- `degenbot-solvers/src/mixed/mod.rs` — `ResolvedHop::V3/V4 { prepared }`, accessors, `as_prepared_cl_hop`.
- `degenbot-solvers/src/mixed/solve.rs` — translate at the solve seam (fingerprint fold moves to derivation).
- `degenbot-solvers/examples/rayon_scale_probe.rs` — fixture constructor.
- `degenbot-substrate/src/resolve/cl.rs` — the projection constructs `PreparedClHop`.
- `degenbot-substrate/src/resolve/mod.rs` — test helpers (`all_cl_solve`/`mixed_solve`, `profile_ptr`/`crossing_ptr`) read the value.
- `degenbot-bot/src/arb_engine/{executor_ab_probe.rs,workload_partition.rs,solver_capture.rs}` — fixture constructors + the CL capture pattern.

Existing tests passed UNMODIFIED (no assertion, expected value, or golden changed). Mechanical
constructor updates were required by the field-shape change at four test/fixture sites
(`workload_partition.rs` test, `solver_capture.rs` test, `resolve/mod.rs` test helpers, and the
`rayon_scale_probe` example) and at the bot probe fixture (`executor_ab_probe.rs`); these only
rebuild the same `ResolvedHop::V3` from the same sequence/tables, so no behavior or expectation
changed.

Gates (foreground, run from `rust/`):
- `cargo test -p degenbot-solvers` -> all green (lib 179 passed / 3 ignored; all integration
  binaries green; doc-tests 0).
- `cargo test -p degenbot-substrate` -> all green (lib 355 passed).
- `cargo test -p degenbot-bot` -> all green (lib 714 passed / 3 ignored; integration binaries green).

Acceptance: `rg "ClSolveTables" rust/crates/foundation/degenbot-substrate` -> no matches (exit 1);
`cargo build -p degenbot-substrate` -> Finished. Nothing PENDING. No foreign tree activity
observed (tree clean at start).
