# L557JO — shared test commons, `HopMath::family_label`, constant/visibility hygiene

Status: **DONE.** Tree left dirty for review; nothing committed. Scoped gates green;
no PENDING items.

Epic 2AAFZ7 (SC1). Zero behavior change: the 96 drift-net snapshots and the
golden-parity tolerance test are byte-identical and green.

## 1. Shared hop builders — `tests/common/mod.rs` (new)

Created `rust/crates/engine/degenbot-solvers/tests/common/mod.rs` as the single home
for every hop builder. Moved verbatim (bodies unchanged):

- duplicated across both test binaries: `v2_hop`, `solidly_hop`, `path`,
  `faithful_base` (plus the `ONE_E18` constant both used);
- single-copy family builders from `stable_golden_drift_net.rs`: `weighted_hop`,
  `stable_hop`, `curve_hop`.

Both test files now declare `pub mod common;` and import from it; **no builder
definition remains in any test file** (verified by grep). The builder bodies are
byte-identical to the pre-extraction inline versions, so the constructed paths —
and therefore the pinned `(optimal_input, profit)` snapshots — are unchanged.
`faithful_base` was identical in both files (`/ 1_000_000_000_000_000_000` ==
`/ ONE_E18`); the common copy uses `ONE_E18`.

Two deviations from the brief's parenthetical, both forced and noted here:

- **`pub mod common;` rather than `mod common;`.** Each integration-test binary
  compiles the module separately, and a private module makes a builder that only
  one binary exercises (e.g. `weighted_hop` in the parity binary) a `dead_code`
  error under the workspace's `warnings = "deny"`. A `pub` module in a `pub`
  module keeps rustc's dead-code analysis quiet with **no** `#![allow(dead_code)]`.
- Consequence: `pub` builders trip clippy's `must_use_candidate` and
  `missing_panics_doc`. Handled explicitly, not by suppression: `#[must_use]` on
  every builder and a `# Panics` section on `stable_hop` (the one builder whose
  `calculate_invariant_deployed(...).expect(...)` can panic).

`Case` (the corpus-entry struct, defined identically in both files) was **not**
moved: it is a struct, not a builder, and is outside the settled shape's list.

## 2. `HopMath::family_label()` — one label shape

Added `pub fn family_label(&self) -> String` on `HopMath` in
`src/profit_envelope/derive.rs`. Both duplicate per-family matches are gone:

- `capture.rs` `DegeneratePathEvent::from_hops` now does
  `match slot { None => "unmapped", Some(hop) => hop.family_label() }`;
- `gate.rs`'s degenerate-hop diagnostic now does `let family = hop.family_label();`
  and hoists the reject reason into a single 2-arm match
  (`Cl => classify_cl_rejection`, `_ => family`), logged as its own `reason` field
  beside `family`.

**Unified on the capture.rs shape** (the forensic event path, as recommended):
`v2(r_in=…,r_out=…)`, `cl`, `solidly_volatile(r_in=…,r_out=…)`,
`weighted(b_in=…,b_out=…,w_in=…,w_out=…)`, `reserve_cap(r_out=…)`,
`solidly_stable(r0=…,r1=…,token_in=…)`,
`balancer_stable(amp=…,n=…,i=…,j=…)`,
`curve_stableswap(amp=…,n=…,i=…,j=…)` (plus `unmapped` for a `None` slot, which
lives on the `Option` in `capture.rs`, not on the enum). The gate's older
`BalancerStable(n=2)`-style labels are retired.

Only observable consequence: for a **non-CL** degenerate hop the capture row's
`reject_reason` is now the family label (was the gate's old-style string). No test
or fixture pins a non-CL `family`/`reject_reason` (grep across `rust/` and `src/`
found only `"unmapped"` and `"cl"` assertions); the CL row is unchanged and its
byte-pinned test still passes. Adding a family now touches 4 sites, not 6.

## 3. Visibility / constant hygiene

- **`u512_to_u256_internal` → `narrow_u512_to_u256`** (`mobius_int_exact.rs`): the
  `_internal` name was a lie — the fn is cross-module (and cross-binary). Every
  caller updated: `mixed/solve.rs`, `cl/tests/mod.rs`, the fn's own tests, and the
  `tests/mobius_discriminant_domain.rs` integration import. Also dropped a stale
  doc header that claimed it "returns `U256::ZERO` if the value overflows" and
  justified its `pub` via `_internal` — it panics on overflow, and the accurate
  "Narrow a `U512` to a `U256`" doc already followed. Doc-comment references in
  `degenbot-pools` (`spec_bounds.rs`, `int_v3_hop.rs`) updated to the new name.
  `docs/adr/ADR-012-*.md` still names the old identifier: ADRs are historical
  records and were deliberately left untouched.
- **`SOLIDLY_ONE` / `BALANCER_ONE` deleted** (`derive.rs`, both local `u64 = 1e18`).
  The gate now uses the math leaves' own units: `degenbot_math::solidly::ONE` and
  `degenbot_math::balancer::constants::ONE` (both `U256`, both `1e18`). Values are
  identical, so nothing moved numerically; the gate no longer redefines a leaf's
  fixed-point unit. `BALANCER_AMP_PRECISION` and `SOLIDLY_MIN_SCALED_RESERVE` were
  left as-is (not in scope).
- **`bounded_brent`**: added a "Role" paragraph to the module docs — a SciPy-1.17
  parity **reference**, never called on the solve path, used only as the CL
  refine-window oracle by tests (`cl/tests/refine_undersample.rs`,
  `tests/bounded_brent.rs`). No feature-gating, no move. `lib.rs`'s module list
  carries no per-module doc lines, so no `lib.rs` change was needed.
- **`VerifySweep` and the two sweep orders: untouched** (SC3's call).

## Gates run

- `cargo nextest run --locked --manifest-path rust/Cargo.toml -p degenbot-solvers`
  → **241 passed, 3 skipped** (includes `stable_golden_drift_net`'s 96 pinned
  snapshots and `solidly_golden_parity`'s tolerance test).
- `cargo clippy --locked --manifest-path rust/Cargo.toml -p degenbot-solvers
  --all-targets -- --deny warnings` → clean.
- `cargo fmt --manifest-path rust/Cargo.toml -p degenbot-solvers -- --check` →
  clean.

Not run (dispatcher/manager scope): `just test-rust-nextest` (whole workspace),
whole-tree lint sweeps. The degenbot-pools edit is doc-comment-only and was
compiled as a dependency of the clippy run.

## PENDING

None.

## Foreign tree activity (left untouched)

- `GLOSSARY.md` and `rust/.../profit_envelope/mod.rs` — doc cross-links added by
  the concurrent DOCS worker (YRYKNP).
- `docs/architecture/stableswap-gate-bounds-and-solve-method-evaluation.md` (new)
  and `docs/ergo-results/yryknp-result.md` (new) — the same worker.
