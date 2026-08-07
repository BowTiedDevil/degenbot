Convert `BotState::register_v2_pool` from `-> u64` to
`-> Result<u64, RegisterV2PoolError>`. Replaces the prior `assert!`
duplicate-check panic with `Err(RegisterV2PoolError::AlreadyRegistered {
address })`, and calls `validate_v2_reserve` (from ZOICEZ) on both
`reserve0` and `reserve1` to enforce the on-chain `uint112` storage width
v2-core asserts at `UniswapV2Pair._update`.

## Files touched

- `rust/crates/degenbot-bot/src/bot_core/mod.rs` —
  - `register_v2_pool` signature + body (`-> Result`; the `assert!`
    replaces with `AlreadyRegistered` + the spec-validation calls;
    returns `Ok(pool_id)`).
  - `RegisterV2PoolError` enum gained a `From<SpecViolation>` impl (for
    `?`-propagation across the spec-validation helper's `Result<(), …>`
    return).
  - 3 new RED/GREEN tests:
    `register_v2_pool_rejects_duplicate_address_as_already_registered`,
    `register_v2_pool_rejects_overlarge_reserve0_as_spec_violation`,
    `register_v2_pool_rejects_overlarge_reserve1_as_spec_violation`.
  - 12 existing test callsites updated to `.expect("test setup: V2
    registration")` (single-line `&make_params(...)` and `&params)` form).
- `rust/crates/degenbot-bot/src/bot_core/{balancer_weighted_state,
  balancer_stable_state,curve_state,block_pump,reorg_coordinator}.rs` —
  test helper `register_v2_pool(&RegisterV2PoolParams {…});` callsites
  → `register_v2_pool(&RegisterV2PoolParams {…}).expect("test setup: V2
  registration");`. (All five are `#[cfg(test)]` helpers — no production
  `?`-cascade was needed.)
- `rust/crates/degenbot-bot/src/solvers/arb_engine/mod.rs` —
  `#[cfg(test)] ArbitrageEngine::register_v2_pool` engine test helper keeps
  `-> u64` transitively via `.expect("test setup: V2 registration")`
  internally, so the ~50 test callsites in `arb_engine/tests.rs`
  stay unchanged.
- `rust/crates/degenbot-bot/src/solvers/arb_engine/tests.rs` —
  5 direct `core.write().register_v2_pool(&…)` sites (multi-line +
  single-line forms) → `.expect("test setup: V2 registration")`.
- `rust/crates/degenbot-bot/src/bot_core/log_dispatcher.rs` — 2 statement-
  form test sites (`state.write().register_v2_pool(…);`) → `.expect(…)`.
- `rust/crates/degenbot/examples/standalone_consumer.rs` —
  `bot.register_v2_pool(&params);` → `.expect("standalone: register
  V2")`.
- `rust/crates/degenbot-python/src/bot/mod.rs` — the `PyBot.register_v2_pool`
  pyfunction: replaces `Ok(self.bot.state_arc().write().register_v2_pool(…))`
  with `self.bot.state_arc().write().register_v2_pool(…).map_err(|e|
  PyValueError::new_err(format!("V2 pool registration failed: {e:?}")))`.
  Stop-gap: the proper typed `PoolRegistrationError` exception hierarchy
  (mirroring `map_register_v4_err`) lands in F2EVV6.

## TDD

RED: 3 new tests asserting the typed `Err` variant on (a) duplicate
address, (b) `reserve0 > uint112::MAX`, (c) `reserve1 > uint112::MAX`.
Confirmed failing to compile against the prior `-> u64` signature.

GREEN: signature + impl + cascade landing. All `cargo test` green.

## Validation

- `cargo test -p degenbot-bot --lib` — 362 passed, 0 failed (359 prior
  baseline + 3 new MSTAT2 tests).
- `cargo test -p degenbot_rs` — 1 integration doc-test passes; the
  `degenbot_rs` lib builds.
- `just test-python`: green — the 300 wrapped `tests/rust` tests pass within
  the full suite.
- `cargo clippy --workspace --all-targets` — clean.
- `cargo fmt --all -- --check` — clean.
- `just check-no-pyo3-in-cores` — OK (no `pyo3` added to a core crate;
  the `PyValueError` stop-gap mapper lives in `degenbot-python/src/bot/`,
  the pyo3-allowed wrapper layer).

## Sequencing note for F2EVV6

F2EVV6 will replace the `PyValueError` stop-gap mapper with a proper
`map_register_v2_err` typed Python exception (mirroring
`map_register_v4_err`'s WOYYS2-finished shape) and add the
corresponding `map_register_v3_err`. The Rust core types
`RegisterV2PoolError` / `RegisterV3PoolError` (landed in ZOICEZ + MSTAT2
+ the upcoming 24KNGF) are stable for that.
