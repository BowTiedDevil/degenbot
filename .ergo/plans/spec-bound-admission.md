# Spec-validation helpers + RegisterV2/V3 error enums

Define `RegisterV2PoolError` and `RegisterV3PoolError` enums mirroring
`RegisterV4PoolError`'s shape. Variants: `AlreadyRegistered { address }`
(replacing the existing `assert!` duplicate-check panic on V2/V3) and
`SpecViolation { field, value, bound }` (carrying the failing field name
e.g. `"reserve0"`, the actual value, and a human-readable bound string).
Implement `std::error::Error` + `Display`. Place each next to its
`Params` struct (`v3_state.rs` for V3; the V2 family lives in
`bot_core/mod.rs`); re-export from `bot_core/mod.rs`.

Also add a pure spec-validation helper module under `bot_core` exposing:
`validate_v2_reserves(reserve0, reserve1) -> Result<(), RegisterV2PoolError>`,
`validate_v3_sqrt_price(sp) -> Result<()>`,
`validate_v3_tick(tick) -> Result<()>`,
`validate_v3_fee(fee) -> Result<()>`,
`validate_v3_tick_spacing(spacing) -> Result<()>`, plus the V4 forms
(reusing `degenbot_cl_math::cl_lib::tick_math::{MIN_SQRT_RATIO,
MAX_SQRT_RATIO, MIN_TICK, MAX_TICK}`). Add a `UINT112_MAX: U256 = (1 <<
112) - 1` constant alongside the V2 helper.

Bounds are from on-chain Solidity types (researched against Uniswap
v2-core / v3-core / v4-core):
- V2 reserves `uint112` -> `<= 2^112 - 1` (v2 `require(balance <=
  uint112(-1))` at `_update`).
- V3/V4 `sqrtPriceX96` `uint160` -> `[MIN_SQRT_RATIO, MAX_SQRT_RATIO)`
  (TickMath rejects out-of-range).
- V3/V4 `liquidity` `u128` -> `<= u128::MAX` (Rust type already enforces;
  no check needed).
- V3 `fee` `uint24` -> `< 1_000_000` (V3 factory).
- V3/V4 `tick` `int24` -> `[MIN_TICK=-887272, MAX_TICK=887272]`.
- V3/V4 `tickSpacing` -> `[1, 32_767]` (V4 spec).

## Acceptance Criteria
- Both error enums compile, derive `Debug` + `Clone` (or hold non-`Clone`
  fields behind `Box`), implement `Display` + `std::error::Error`, and
  are re-exported from `bot_core::mod`.
- All spec helpers exist with the signatures above and return
  `Err(SpecViolation)` on out-of-bounds input.
- `UINT112_MAX` constant defined.
- Red/Green: boundary tests covering accept (`== MAX` inclusive where
  on-chain allows) and reject (`MAX + 1`) for each helper, plus a
  duplicate-address variant test on the enums.

## Validation
- `cargo test -p degenbot-bot --lib bot_core` green
- `cargo clippy -p degenbot-bot --all-targets` clean
- `cargo fmt -p degenbot-bot --check` clean

---

# Convert register_v2_pool to Result + V2 cascade

Change `BotState::register_v2_pool` from `-> u64` to
`Result<u64, RegisterV2PoolError>`. Replace the duplicate-check `assert!`
with `Err(RegisterV2PoolError::AlreadyRegistered { address })` and prepend
a `validate_v2_reserves(reserve0, reserve1)?` call using the helper from
the previous task.

Update the non-test callsites (propagate via `?`):
`rust/crates/degenbot-bot/src/bot_core/log_dispatcher.rs` (V2 register
callsites), `reorg_coordinator.rs`, `block_pump.rs`,
`balancer_weighted_state.rs`, `curve_state.rs`,
`balancer_stable_state.rs` — exact line numbers from `.ergo/plan-...`
audit; grep before editing to refresh.

Update `UniswapEngine::register_v2_pool` `#[cfg(test)]` engine helper
(in `solvers/uniswap_engine/mod.rs`) to call the core fn and
`.expect("test setup: V2 registration")` internally, keeping its `-> u64`
signature so the ~50 test callsites in `uniswap_engine/tests.rs` remain
unchanged. Update `rust/crates/degenbot/examples/standalone_consumer.rs`
with `.expect("standalone: register V2")`.

## Acceptance Criteria
- `register_v2_pool` returns `Result<u64, RegisterV2PoolError>`; no `assert!`
  remains in the function body.
- All non-test callsites compile (use `?`).
- The `#[cfg(test)]` engine helper preserves `-> u64`.
- Red: tests for an over-large `reserve0`/`reserve1` and a duplicate
  address assert the typed `Err` variant (not a panic).
- Green: implementation passes after the Red test lands.

## Validation
- `cargo test -p degenbot-bot` green
- `cargo clippy -p degenbot-bot --all-targets` clean
- crates `degenbot` (standalone_consumer) + `degenbot-bot` compile clean

---

# Convert register_v3_pool to Result + V3 cascade

Same shape as V2 but with `RegisterV3PoolError` (from task 1) and the V3
spec helpers: `validate_v3_sqrt_price(sqrt_price_x96)?`,
`validate_v3_tick(tick)?` (where present on the params),
`validate_v3_fee(fee)?`, `validate_v3_tick_spacing(tick_spacing)?`.

Update non-test callsites (propagate via `?`):
`rust/crates/degenbot-bot/src/bot_core/log_dispatcher.rs` (V3 register
callsite), `reorg_coordinator.rs`. Update the
`UniswapEngine::register_v3_pool` `#[cfg(test)]` helper to
`.expect("test setup: V3 registration")`, preserving `-> u64`. Grep to
refresh line numbers before editing.

## Acceptance Criteria
- `register_v3_pool` returns `Result<u64, RegisterV3PoolError>`; no
  `assert!` remains in the function body.
- All non-test callsites compile (use `?`).
- The `#[cfg(test)]` engine helper preserves `-> u64`.
- Red: tests for `sqrt_out_of_range` (`MAX_SQRT_RATIO` and below
  `MIN_SQRT_RATIO`), `tick_out_of_range`, `fee_too_large`
  (`>= 1_000_000`), and `tick_spacing_out_of_range` assert the typed
  `SpecViolation` variant.
- Green: implementation passes.

## Validation
- `cargo test -p degenbot-bot` green
- `cargo clippy -p degenbot-bot --all-targets` clean

---

# Extend register_v4_pool with V4 spec checks

`register_v4_pool` already returns `Result<u64, RegisterV4PoolError>` and
already rejects `HookedPool` / `DynamicFee` (static-fee guard) /
`AlreadyRegistered`. Extend `RegisterV4PoolError` with a
`SpecViolation { field, value, bound }` variant matching the V2/V3
shape (task 1) — reuse the same struct if V2/V3's `SpecViolation` lives
in a shared module; otherwise define a parallel V4 form consistent with
the codebase's existing V4 error-shape conventions.

Add V4 spec-helper checks at the top of `register_v4_pool` alongside the
existing rejections: sqrt bounds via `validate_v4_sqrt_price`, tick
bounds via `validate_v4_tick` (where present), `fee < 1 << 24`, and
`tick_spacing ∈ [1, 32_767]`.

## Acceptance Criteria
- `RegisterV4PoolError` gains a `SpecViolation` variant.
- `register_v4_pool` calls the V4 spec helpers before the existing
  `HookedPool` / `DynamicFee` / `AlreadyRegistered` rejections.
- Red: tests for `sqrt_out_of_range`, `tick_out_of_range`, `fee_too_large`
  (`>= 1 << 24`), `tick_spacing_out_of_range` assert the V4
  `SpecViolation` variant.
- Green: implementation passes.
- Existing `map_register_v4_err` callers (degenbot-python) still compile.

## Validation
- `cargo test -p degenbot-bot` green
- `cargo clippy -p degenbot-bot --all-targets` clean

---

# PyO3: typed-error mappers + Python register_v2/v3 bindings

In `rust/crates/degenbot-python/src/bot/engine/register.rs`, extend
`map_register_v4_err` to also handle the new `SpecViolation` variant, and
add `map_register_v2_err`/`map_register_v3_err` mapping
`AlreadyRegistered` and `SpecViolation` to typed Python exceptions
(mirror the existing V4 mappings — `DegenbotValueError` for spec
violations if that pattern already exists, else a new
`PoolSpecError`/`PoolRegistrationError`). Update the `register_v2_pool`
and `register_v3_pool` pyfunctions in
`rust/crates/degenbot-python/src/bot/mod.rs` to unwrap the `Result` via
the new mappers (`?` + `From<RegisterV{2,3}PoolError> for PyErr`).

Add `.pyi` stub entries for the new exception type if introduced.

## Coordination constraint

t-5092 (Rust Solver Explainooooor's counterpart on the web3py/sim-seam
epic) is idle on Pass B/C of the retire-web3py epic pending user
direction. Send a `link_send(triggerTurn: true)` to "t-5092" before
editing `rust/crates/degenbot-python/src/bot/{mod.rs,engine/register.rs}`
to confirm the wrapper layer is free; abort this task and stay in `todo`
if t-5092 is mid-edit on those files.

## Acceptance Criteria
- `register_v2_pool`/`register_v3_pool` pyfunctions surface a typed Python
  exception (NOT `PanicException`) on out-of-spec input.
- `map_register_v2_err` / `map_register_v3_err` / updated
  `map_register_v4_err` return typed Python errors.
- Red: Python test (`rust/crates/degenbot-python/tests/` or `tests/rust/`)
  registering an out-of-spec V2/V3 pool via the Python seam asserts the
  typed Python exception type and message.
- Green: implementation passes.

## Validation
- `just test-rust-python` green
- `just lint-rust` green
- `cargo build -p degenbot-python` clean

---

# Final audit + ADR note

Sweep audit:
- Confirm no remaining `assert!` in the three register fns.
- Re-evaluate the stop-gap `expect` messages in `u512_to_u256_internal`
  and `IntHopState::swap` (committed in `19218a2c`) — they should still
  read as invariant assertions but cite the registration-time spec
  enforcement as the upstream guarantee.
- `cargo test -p degenbot-bot -p degenbot-python` green.
- `cargo clippy --workspace --all-targets` clean.
- `just lint-rust` green.

Add a brief ADR-005 / ADR-003 follow-up note under `docs/adr/`
documenting the spec-bound admission contract: registration is the
spec-verification seam; no downstream swap math re-verifies spec bounds,
so callers can rely on the narrowing helper's assertion confidently. If
a new typed Python exception was added, document its type/hierarchy in
the ADR.

## Acceptance Criteria
- Zero `assert!` in `register_v2_pool` / `register_v3_pool` /
  `register_v4_pool` (verified by `grep`).
- The two narrowed-helper `expect`/`assert!` messages updated to cite
  registration-time enforcement.
- A new ADR (or ADR-005/003 addendum) records the seam contract.
- Full workspace `cargo test` + `just lint-rust` green.

## Validation
- `just test-rust` + `just test-rust-python` green
- `just lint-rust` clean