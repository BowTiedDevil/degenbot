# YEX5AJ (ADR-071 T2) — review-fix round: fn-pointer dispatch-row test pin



Review-fix round on the T2 diff already in the tree (previous worker's

uncommitted `degenbot-solvers` changes; that work is the baseline and was left

untouched). This round is TEST-SIDE ONLY plus one small const, per the manager's

two findings.



## What changed



**`rust/crates/engine/degenbot-solvers/src/mixed/solve.rs`** — the test module

`mod gate_tests` only (nothing outside `#[cfg(test)]`).

- The per-family assertions in

  `family_dispatch_table_selects_the_row_the_old_ladder_selected` are now

  POSITIVE identity, not "not the refusal stub" (Finding 1). New module-level

  helper `expected_entry(ht) -> (&'static str, ToMathFn, LinesFn, SolveFn)`

  returns the EXACT `(label, to_math, lines, solve)` each family's row must

  register, and the test asserts

  `std::ptr::fn_addr_eq(row.to_math, to_math)` (and `.lines`, `.solve`) against

  it. A cross-wired LIVE builder (e.g. the CL row's `lines` pointing at

  `weighted_hop_lines`) now fails immediately.

- Coverage gap closed mechanically (Finding 2): `expected_entry` is an

  exhaustive `match` with NO wildcard arm, so adding a `HopType` variant fails

  COMPILATION of the test until the new variant's registration is named.

  `assert_all_variants_listed()` additionally guards that `HopType::ALL` lists

  every variant exactly once (its own exhaustive or-pattern match + count

  assertion). The test iterates `HopType::ALL` and drives `expected_entry` per

  variant.

- The existing selection cases (the ladder pin) are untouched.



**`rust/crates/engine/degenbot-solvers/src/mixed/mod.rs`** — one small const in

the `HopType` impl (explicitly permitted by the brief):

`pub const ALL: &[HopType] = &[V2, V3, V4, SolidlyStable, BalancerWeighted,

BalancerStable, CurveStableswap];`. Production code otherwise byte-identical.



## Cross-wire mutation probe (Finding 1 acceptance, on the FINAL code)



Baseline checksum (pre-mutation):

`fd9274f7f155aff619d3fd015e963839f88f6f192afe4c111a3884ade1f533ed  solve.rs`



Mutation: CL row `lines: cl_hop_lines` -> `lines: weighted_hop_lines` (a live,

non-refusal builder — exactly the class the pin exists to catch).



```

test mixed::solve::gate_tests::family_dispatch_table_selects_the_row_the_old_ladder_selected ... FAILED

---- ... stdout ----

thread '...' panicked at crates/engine/degenbot-solvers/src/mixed/solve.rs:1938:13:

V3 entry row must register the family's line builder, not a cross-wired or refusal pointer

test result: FAILED. 0 passed; 1 failed; 0 ignored; 183 filtered out

```



Restore: reverted the one-token edit; checksum re-verified EQUAL to the

baseline `fd9274f7...`; targeted test then `ok`.



## Gates run (all foreground, scoped)



- `cargo fmt --manifest-path rust/Cargo.toml -p degenbot-solvers -- --check` — green.

- `cargo clippy --manifest-path rust/Cargo.toml -p degenbot-solvers --all-targets -- -D warnings` — green.

- `cargo test --manifest-path rust/Cargo.toml -p degenbot-solvers` — green

  (lib 181 passed / 3 ignored, plus all integration suites; 0 failed).

- Targeted `--lib family_dispatch_table_selects_the_row_the_old_ladder_selected`

  — green after restore.



Not run (dispatcher-owned): `just test-rust-nextest`, `just test-python`,

`just test-rust`, whole-tree lint/format sweeps.



## PENDING



None.



## Foreign activity



None beyond the brief. Tree dirty set: `mixed/mod.rs` (my `ALL` const),

`mixed/solve.rs` (previous worker's T2 baseline + my test-module fix),

`profit_envelope/derive.rs` and `profit_envelope/mod.rs` (previous worker's T2

baseline). All my solve.rs hunks are inside `mod gate_tests` (git hunk headers

at `@@ -1618,3 +1773,41 @@ mod gate_tests` and `@@ -1622,12 +1815,38 @@ mod gate_tests`).

