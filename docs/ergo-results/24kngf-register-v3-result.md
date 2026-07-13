# 24KNGF — `register_v3_pool → Result` + V3 cascade

## Outcome
`BotState::register_v3_pool` is now `Result<u64, RegisterV3PoolError>`. Four V3 spec validators fire up-front (before the `seed_from_store` snapshot step — `seed_from_store` only overrides `tick_data`/`coverage` and never touches the validated immutable-config / current-state scalars):

- `validate_sqrt_price(params.sqrt_price_x96)` (`TickMath`-bounded `<uint160, MAX_SQRT_RATIO>`)
- `validate_tick(params.tick)` (`int24`, `[MIN_TICK, MAX_TICK]`)
- `validate_v3_fee(params.fee)` (`< 1_000_000`, V3 factory)
- `validate_tick_spacing(params.tick_spacing)` (`[1, 32_767]`)

The `assert!` duplicate-address panic becomes `Err(RegisterV3PoolError::AlreadyRegistered { address })`.

## Cascade
Same shape as MSTAT2 (V2). The `~50 test callsites` went through one of three paths:

- `UniswapEngine::register_v3_pool` `#[cfg(test)]` engine helper: now `self.core.write().register_v3_pool(params).expect("test setup: V3 registration")`, preserving `-> u64`. ~30 test callsites in `uniswap_engine/tests.rs` + 2 in `diagnostic.rs` go through this helper and remain unchanged.
- `BotState::register_v3_pool` direct test calls in `mod.rs` (14), `log_dispatcher.rs` (1), `reorg_coordinator.rs` (1), `uniswap_engine/tests.rs` (1): patched with `.expect("test setup: V3 registration")`. A Python-bracket-walk script handled the multi-line `&RegisterV3PoolParams { ... }` literal form + the helper-defn tail (no trailing `;`) specially.
- PyO3 wrapper `degenbot_python::bot::mod::PyBot::register_v3_pool`: stop-gap `.map_err(|e| PyValueError::new_err(format!("V3 pool registration failed: {e:?}")))`. The typed `map_register_v3_err` Python exception hierarchy is task **F2EVV6** (it bumps the V3 wrapper, the V2 wrapper, and introduces the shared `PoolRegistrationError` base class all together).

## Red/Green
7 new tests in `bot_core::tests`:
- `register_v3_pool_rejects_duplicate_address_as_already_registered`
- `register_v3_pool_rejects_sqrt_price_at_max_as_spec_violation`
- `register_v3_pool_rejects_sqrt_price_below_min_as_spec_violation`
- `register_v3_pool_rejects_tick_below_min_as_spec_violation`
- `register_v3_pool_rejects_fee_at_max_as_spec_violation`
- `register_v3_pool_rejects_tick_spacing_out_of_range_as_spec_violation`
- `register_v3_pool_accepts_in_spec_params` (green companion at every validator's accept boundary)

## Validation
- `cargo test -p degenbot-bot --lib`: 369 (was 362; +7 new), all green.
- `cargo clippy -p degenbot-bot --all-targets`: clean.
- `cargo fmt -p degenbot-bot -p degenbot_rs --check`: clean.
- `cargo build --example standalone_consumer`: clean.
- `just check-no-pyo3-in-cores`: OK (no `pyo3` in core crates; only the `degenbot-python` wrapper touches `pyo3`).
- `cargo test -p degenbot_rs --test python_integration`: 16/16 green.
- `uv run pytest tests/ -k "register_v3 or v3_pool or v3register"`: 32 passed, 4 skipped.

## Followups
- **F2EVV6** (blocked-by this task): replace the V2 + V3 stop-gap `PyValueError` mappers with the typed `PoolRegistrationError` hierarchy + `map_register_v2_err` / `map_register_v3_err` (mirroring `map_register_v4_err`). The stop-gap surfaces `RegisterV{2,3}PoolError` to Python as an opaque `PyValueError({:?})`; the typed mapper will give Python a class hierarchy to catch against, matching the V4 precedent.
- **RNZQUO** (final audit + ADR note) still gated on K3IICB + F2EVV6.
