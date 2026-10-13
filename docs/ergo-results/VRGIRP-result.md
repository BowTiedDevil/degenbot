## Result — VRGIRP (ADR-071 T5: weighted / balancer-stable / curve family modules)

Landed on the working tree (uncommitted; manager owns commits). HEAD is
`b9fba6e59` at sign-off, unchanged from claim.

### What changed

Three new family modules under `rust/crates/engine/degenbot-solvers/src/mixed/families/`,
mirroring `cl.rs` / `solidly.rs`:

- `balancer_weighted.rs` (353 lines)
- `balancer_stable.rs` (557 lines)
- `curve_stableswap.rs` (343 lines)

Everything moved out of `mixed/solve.rs`, `profit_envelope/derive.rs`, and
`mixed/mod.rs` is **byte-identical** — verified mechanically by re-extracting
each of the 34 moved blocks from `git show HEAD:<path>` and asserting
`block in new_file` (all OK), plus a negative check that each block is gone
from its old home. The only intentional deltas are the ones `REWO4K` also made:

- visibility widening `fn` -> `pub(crate) fn` on the six dispatch-row entry
  points (`weighted_to_math`, `solve_balancer_weighted_entry`, `stable_to_math`,
  `solve_balancer_stable_entry`, `curve_to_math`, `solve_curve_entry`) — the row
  and the pin need them reachable from `mixed::solve`;
- the three `solve_*_entry` docs reworded from "`[`SolveFn`]` adapter ..." to
  the Solidly phrasing (drops a `[`SolveFn`]` intra-doc link that would have
  needed a use-less import);
- the re-export / import wiring.

Moved into each module:

- **weighted**: `BalancerWeightedHopState` (from `mixed/mod.rs`),
  `weighted_to_math`, `solve_balancer_weighted_entry` +
  `solve_balancer_weighted_path_int`, `balancer_weighted_hop_v2_equiv`,
  `simulate_balancer_weighted_path` / `_outputs` / `_hop`, `weighted_hop_lines` /
  `weighted_lines`.
- **balancer-stable**: `BalancerStableHopState`, `stable_to_math`,
  `solve_balancer_stable_entry` + `solve_balancer_stable_path_int`,
  `balancer_stable_hop_v2_equiv`, `simulate_balancer_stable_path` / `_outputs` /
  `_hop`, `balancer_stable_hop_lines` / `balancer_stable_lines` /
  `balancer_stable_rise`, `stableswap_concave_at` + `BALANCER_AMP_PRECISION`.
- **curve**: `CurveStableswapHopState`, `curve_to_math`, `solve_curve_entry` +
  `solve_curve_path_int`, `curve_hop_v2_equiv`, `simulate_curve_path` /
  `_outputs` / `_hop`, `curve_hop_lines` / `curve_stableswap_lines`.

### Soundness stances preserved byte-identical

- Curve keeps the RESERVE-CAP-ONLY bound: `curve_stableswap_lines` still returns
  the single flat native cap line (`[flat]`), with the "measured non-concave
  leaf" doc moved verbatim. No entry tangent.
- Balancer-stable keeps the entry tangent ONLY where `stableswap_concave_at`
  holds, beside the reserve cap (`balancer_stable_lines` = `[rise, flat]` or
  `[flat]`); the curvature test and the faithful-arithmetic window guard moved
  verbatim with it.
- Balancer-weighted keeps the concave entry tangent + asymptotic reserve cap.

### Dispatch wiring / pins

- The three `FAMILY_DISPATCH` rows now point at the family modules'
  `solve_*_entry` / `*_to_math` / `*_hop_lines`.
- The pin `family_dispatch_table_selects_the_row_the_old_ladder_selected` keeps
  positive fn identity: `expected_entry` names the same three moved fns
  (imported from `crate::mixed::families::{balancer_stable, balancer_weighted,
  curve_stableswap}`), and the `std::ptr::fn_addr_eq` assertions pass.
- `mixed/mod.rs` re-exports the three state slices at their historical path
  (`degenbot_solvers::mixed::{BalancerWeightedHopState, BalancerStableHopState,
  CurveStableswapHopState}`), so `degenbot-substrate`, `tests/common`, and the
  benches compile unchanged; no duplicate re-exports. `PowVersion` and
  `{DVariant, YVariant}` imports dropped from `mixed/mod.rs` (only the moved
  structs used them).
- `profit_envelope/mod.rs` re-exports reduced to the shared / residual builders
  (`v2_hop_lines`, `reserve_cap_hop_lines`, `refuse_lines`); `mobius_lines`
  stays.

### D6 test relocation (relocate, never delete)

- `m6776w_curve_reserve_cap_dominates_real_leaf` (proptest) -> `curve_stableswap.rs`.
- `m6776w_balancer_weighted_bound_dominates_real_leaf` (proptest) ->
  `balancer_weighted.rs`.
- The `proptest!` block, its `use proptest::prelude::*;` and
  `use degenbot_math::balancer::constants::ONE;` left `solve.rs`'s `gate_tests`
  with the two tests.
- Balancer-stable bound case: there are no balancer-stable cases in `mod
  gate_tests`; the only balancer-stable *bound-shape* case not riding the shared
  `DominanceFamily` harness was `balancer_stable_curvature_guard_shape` in
  `tests/profit_envelope_tests.rs`. The first pass DUPLICATED rather than moved
  it — the integration copy survived at `tests/profit_envelope_tests.rs:1803`
  while a second copy landed in `balancer_stable.rs` (the integration binary
  stayed at 27 tests; the lib grew by one). A review-fix round completed the
  relocation: the integration copy's unit-input-bound assertions
  (`bound < reserve` inside the curvature domain, `bound == reserve` for the
  wrap-window pool — exercised through the public `hop_lines_and_cap` path) were
  ported onto the family copy, and the stale integration copy was deleted. The
  surviving `balancer_stable.rs` unit test asserts the derived line-set shape
  directly against the family's private `balancer_stable_lines` (rise+cap inside
  the curvature domain, cap-only for the wrap-window pool) — the family module's
  interface is the test surface (D6), exactly mirroring `REWO4K`'s relocation of
  `solidly_stable_window_guard_...` from `derive.rs`.
- Left in place, deliberately (per D6): the cross-family `DominanceFamily`
  harness + `run_dominance_suite` in `tests/profit_envelope_tests.rs` (its
  `balancer_stable_entry_tangent_dominates_real_leaf` instantiation is the
  shared pin), the Curve non-concavity counterexamples there, the shared corpus
  in `tests/common/`, the gate-screening assertion
  `solidly_and_stableswap_hops_are_now_screened_not_unsupported`, and the pin
  itself in `solve.rs`'s `gate_tests`.

### Acceptance criteria

- Byte-identical solves and bounds: moved-code verbatim check passes; parity
  corpus + `stable_golden_drift_net` (all four arms) + `solidly_golden_parity` +
  the m6776w regressions green unchanged.
- `mixed/solve.rs` retains ONLY shared kernels (golden template, verification
  order, ascending sweep, dispatch table + pin, `solve_path*`): no
  weighted/stable/curve simulation, projection, or lines functions remain.
- `profit_envelope/derive.rs` retains no per-family line builders — only
  `HopMath` (+ `family_label`), `mobius_lines`, the `v2_hop_lines` /
  `reserve_cap_hop_lines` / `refuse_lines` builders, the caches, and the
  one-line row delegations.
- The pin stays green asserting the family modules' fns.

### Gates (all foreground, all green)

- `cargo fmt --manifest-path rust/Cargo.toml -p degenbot-solvers -- --check` -> 0
- `cargo clippy --manifest-path rust/Cargo.toml -p degenbot-solvers --all-targets -- -D warnings` -> clean
- `cargo test -p degenbot-solvers` -> all green (lib 182 passed / 3 ignored;
  profit_envelope 27; solidly_golden_parity 1; stable_golden_drift_net 4;
  cl_cache_lab_goldens; offline_heavy_replay; the V3/V4 parity binaries; 0 failed)
- `cargo check --workspace --all-targets` -> Finished

Nothing PENDING.

### Observed, NOT touched (foreign / out of scope)

- `docs/architecture/adding-a-hop-family.md` still describes the old eight-site
  shape — that is the ADR-071 D7 conductor rewrite (T6), not this chunk.
- Open solver bugs OJ2MMF / YYHWVQ / A47GNH untouched; the CP profitability
  precheck is invoked by these arms through the shared template, unmodified.
- No foreign commits and no foreign working-tree edits observed. HEAD is
  `b9fba6e59`; the tree carries only the five modified solver files + the three
  new family modules + this result file.
