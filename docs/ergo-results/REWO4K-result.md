## Result — REWO4K (ADR-071 T4: the Solidly family module)

Landed on the working tree (uncommitted; manager owns commits). HEAD was
`fb4a0f2c4` at claim and is still `fb4a0f2c4` at sign-off.

### What changed

One new family module, `rust/crates/engine/degenbot-solvers/src/mixed/families/solidly.rs`
(812 lines), mirroring `cl.rs`. Everything moved out of `mixed/solve.rs` /
`profit_envelope/derive.rs` / `mixed/mod.rs` is **byte-identical** — verified
mechanically by re-extracting each block from `git show HEAD:<path>` and
asserting `block in new_file` (all 15 blocks OK, incl. the four relocated
tests). The only intentional deltas are visibility (`fn` → `pub(crate) fn` on
`solidly_to_math`), the dispatch-row adapter (body byte-identical; `fn` →
`pub(crate) fn`, doc reworded to drop a `[`SolveFn`]` link that would have
needed a use-less import), and the re-export/import wiring.

Moved into the family module:
- `SolidlyHopState` (state slice; from `mixed/mod.rs`)
- `solidly_to_math` (gate projection)
- `solve_solidly_path_int` + `solve_solidly_entry` (row adapter)
- `solidly_hop_v2_equiv` (precheck projection)
- `simulate_solidly_path` / `simulate_solidly_path_outputs` / `simulate_solidly_hop`
- `solidly_hop_lines` / `solidly_stable_lines` / `solidly_stable_rise` (bounds)

Family-internal split kept as the two `HopMath` arms: volatile = constant-product
(fee-agnostic V2 lines through the shared `mobius_lines`), stable = concave entry
tangent beside the reserve cap. Both byte-identical.

Per ADR-071 D4, the single residual enum sites stay central:
`HopMath::family_row()` (one line per family into the dispatch table) and the
`ResolvedHop` / `HopMath` variants are untouched — the task's own D4 text names
`family_row()` as accepted (manager disposition). `family_label` stays on
`HopMath` for the same reason.

### Dispatch wiring / pins
- `FAMILY_DISPATCH` Solidly row now points at the family module's
  `solve_solidly_entry` / `solidly_to_math` / `solidly_hop_lines`.
- The pin `family_dispatch_table_selects_the_row_the_old_ladder_selected` keeps
  positive fn-identity: `expected_entry(HopType::SolidlyStable)` names the same
  three moved fns (imported from `crate::mixed::families::solidly`), and the
  `std::ptr::fn_addr_eq` assertions pass.
- `mixed/mod.rs` re-exports updated, no duplicates:
  `pub use families::solidly::{simulate_solidly_path, SolidlyHopState};`
  + `pub use solve::{solve_path, solve_path_with_min_profit};`.
  Downstream (`degenbot-substrate`, `degenbot-bot` tests, `tests/common`,
  benches) still reach `degenbot_solvers::mixed::{SolidlyHopState,
  simulate_solidly_path}` unchanged.
- `solve_golden_section_path_int` (shared template, family-blind) widened to
  `pub(crate)` so the family module can call it; `mobius_lines` likewise
  `pub(crate)` and re-exported through `profit_envelope` (V2 + Solidly-volatile
  both call it; it stays in `derive.rs`).

### D6 test relocation
Relocated into `mixed/families/solidly.rs`'s `mod tests` (never deleted):
- `m6776w_solidly_volatile_fee_floor_counterexample_is_bounded`
- `solidly_stable_tangent_prunes_where_reserve_cap_cannot`
- proptests `m6776w_solidly_volatile_bound_dominates_real_leaf`,
  `m6776w_solidly_stable_reserve_cap_dominates_real_leaf`
- `solidly_stable_window_guard_drops_rise_only_past_the_wrap_threshold`
  (from `derive.rs`; it calls `solidly_stable_lines` directly, so it had to move)

Left in place, deliberately: `solidly_and_stableswap_hops_are_now_screened_not_unsupported`
(`solve.rs` `gate_tests`) — it is a gate *screening* assertion whose name spans
Solidly + stableswap, not a Solidly dominance case; and the cross-family Curve /
Balancer-weighted proptests. Shared corpus in `tests/common/` and the
`tests/profit_envelope_tests.rs` Solidly cases are untouched.

Two now-unfulfilled `#[expect]` attributes were dropped as a consequence (their
only firing sites moved out): `#[expect(clippy::panic)]` in `solve.rs`'s
`gate_tests`, `#[expect(clippy::expect_used)]` in `derive.rs`'s `tests`. The
family module's `tests` carries `#[expect(clippy::expect_used, clippy::panic)]`.

### Stale comments fixed in passing
- `mixed/solve.rs`: the `simulate_balancer_weighted_hop` doc said the fallback
  matched "`simulate_solidly_hop`" — qualified to
  `mixed::families::solidly::simulate_solidly_hop`.
- `mixed/mod.rs`: the re-export comment now names the family module as owner of
  the Solidly simulate entry + state slice.
- `mixed/mod.rs`: dropped the now-unused `DexVariant` import (it existed only
  for the moved struct).

### Observed, NOT touched (foreign/out of scope)
- `docs/architecture/adding-a-hop-family.md` still cites `solidly_stable_lines`
  `:345` and `solidly_stable_arithmetic_faithful` — that file is the ADR-071 D7
  conductor rewrite, not this chunk.
- No foreign commits or foreign working-tree edits observed; tree at sign-off
  carries only the five modified solver files + the new family module.

### Gates (all foreground, all green)
- `cargo fmt --manifest-path rust/Cargo.toml -p degenbot-solvers -- --check` → 0
- `cargo clippy -p degenbot-solvers --all-targets -- -D warnings` → clean
- `cargo test -p degenbot-solvers` → all binaries ok (lib 181 passed / 3 ignored;
  profit_envelope 27; solidly_golden_parity 1; stable_golden_drift_net 4 incl.
  `solidly_arm_matches_golden_snapshot`; cl_cache_lab_goldens 1;
  offline_heavy_replay 1; the V3/V4 parity binaries; 0 failed anywhere)
- `cargo check --workspace --all-targets` → Finished (re-export paths across
  `degenbot-substrate`, `degenbot-bot`, benches, examples compile)

Nothing PENDING.

### Acceptance criteria
- Byte-identical solves and bounds: moved-code verbatim check passes; parity
  corpus + `solidly_golden_parity` + drift-net + the m6776w Solidly dominance
  regressions green unchanged. ✓
- `mixed/solve.rs` retains no Solidly simulation/projection/lines functions
  (only the import of the family module's three entry fns, the dispatch row, and
  the pin's `expected_entry` names). ✓
- `profit_envelope/derive.rs` retains no Solidly line-builder code; the row
  points into the family module. ✓
- Pin stays green asserting the family module's fns. ✓
