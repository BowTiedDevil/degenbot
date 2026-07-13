New module: `rust/crates/degenbot-bot/src/bot_core/spec_bounds.rs`

- `SpecViolation { field, value: SpecValue, bound }` shared struct +
  `SpecValue { U256, U128, U32, I32 }` enum (type-erased primitive,
  uniform across u32/i32/u128/U256 field types). Display on both.
- Constants: `UINT112_MAX: U256 = uint!(5192296858534827628530496329220095_U256)`
  (= 2^112 - 1), `V3_FEE_MAX = 1_000_000`, `V4_FEE_MAX = 1 << 24`,
  `MIN_TICK_SPACING = 1`, `MAX_TICK_SPACING = 32_767`.
- Pure validators returning `Result<(), SpecViolation>`:
  `validate_v2_reserve`, `validate_sqrt_price`, `validate_tick`,
  `validate_v3_fee`, `validate_v4_fee`, `validate_tick_spacing`.
  Bounds reuse `degenbot_cl_math::cl_lib::tick_math::{MIN_SQRT_RATIO,
  MAX_SQRT_RATIO, MIN_TICK, MAX_TICK}` (V3/V4 share TickMath).
- `RegisterV2PoolError` enum in `bot_core/mod.rs` next to
  `RegisterV2PoolParams`; re-exported from `bot_core`.
- `RegisterV3PoolError` enum in `bot_core/v3_state.rs` next to
  `RegisterV3PoolParams`; re-exported.
  Both enums: `#[derive(Clone, Debug, PartialEq, Eq)]` with
  `AlreadyRegistered { address }` and `SpecViolation(SpecViolation)`
  variants. No `Display`/`Error` impl — mirrors the existing
  `RegisterV4PoolError` convention (the PyO3 mapper pattern-matches
  the variants directly).
- `spec_bounds` registered as a `pub mod` under `bot_core`.
- Re-exported from `bot_core`: `SpecValue`, `SpecViolation`, `UINT112_MAX`,
  `RegisterV2PoolError`, `RegisterV3PoolError`.

TDD: 21 boundary tests (12 accept + 9 reject) exercising each validator's
edge values (`== MAX` accept, `MAX + 1`/`MAX` (where strict-`<`) reject).
RED confirmed (stubs returned Ok on reject), GREEN impl passes.

Validation: 359 bot lib tests pass; `cargo clippy --workspace --all-targets`
clean; `cargo fmt -p degenbot-bot -- --check` clean; `just
check-no-pyo3-in-cores` PyO3-free; standalone consumer (`cargo build -p
degenbot`) builds.

Forward-reference: the register-fn `-> Result` conversions (MSTAT2, 24KNGF,
K3IICB) consume these helpers via `.map_err(RegisterV*PoolError::SpecViolation)`
and the `AlreadyRegistered` variant replaces the existing `assert!`
duplicate-check panic on V2/V3.
