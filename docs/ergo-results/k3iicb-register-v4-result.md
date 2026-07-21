# K3IICB — V4 spec-bound admission on `register_v4_pool`

## Outcome
`RegisterV4PoolError` gains a `SpecViolation(SpecViolation)` variant (twin of the V2 `RegisterV2PoolError::SpecViolation` from MSTAT2 and V3 `RegisterV3PoolError::SpecViolation` from 24KNGF). It wraps the shared `spec_bounds::SpecViolation` (carrying `field`/`value`/`bound`), and gains the matching `From<SpecViolation>` impl so `?`-propagation from the validator helpers works.

`BotState::register_v4_pool` now prepends the four V4-family spec validators — ahead of the existing `HookedPool` / `DynamicFee` / `AlreadyRegistered` rejections:

- `validate_sqrt_price(params.sqrt_price_x96)` — `[MIN_SQRT_RATIO, MAX_SQRT_RATIO)` (shared `TickMath`-bounded `uint160`, family-agnostic with V3).
- `validate_tick(params.tick)` — `[MIN_TICK, MAX_TICK]` (V3/V4 share the same `TickMath`).
- `validate_v4_fee(params.pool_key.fee)` — `< 1 << 24` (the **V4** static-fee bound; the `0x800000` high bit flags a dynamic-fee pool, separately rejected upstream as the more specific `DynamicFee` variant. `validate_v4_fee(0x800000)` accepts because `8_388_608 < 16_777_216`, so the spec validator never fires on the dynamic-fee value — the `DynamicFee` rejection stays the canonical typed rejection for that case).
- `validate_tick_spacing(params.pool_key.tick_spacing)` — `[1, 32_767]`.

The V4 spec validators run *before* the `seed_from_store` snapshot step (only `tick_data`/`coverage` are snapshot-derived; the validated immutable-config / current-state scalars are not touched by `seed_from_store`) and *before* the existing hook/dynamic rejections, so an impossible-CL-config rejection surfaces the primitive at fault rather than being masked by admission rejection.

## PyO3 mapper (stop-gap)
`map_register_v4_err` (in `degenbot-python/src/bot/engine/register.rs`) already pattern-matched over the `RegisterV4PoolError` enum and now gains the new `SpecViolation` arm — surfacing as a `PyValueError` whose message mirrors `SpecViolation`'s `Display` (`"field <name> value <val> out of bounds: <bound>"`). The typed `PoolRegistrationError` Python exception hierarchy (with a `SpecViolationError` typed for V2/V3/V4, plus the V2/V3 twins gaining their `map_register_v2_err` / `map_register_v3_err`) is task **F2EVV6** — it bumps all three wrappers + a new shared base class together so all three reach the typed PyO3 mapper in lockstep.

## Red/Green
6 new tests in `bot_core::tests`:

- `register_v4_pool_rejects_sqrt_price_at_max_as_spec_violation`
- `register_v4_pool_rejects_sqrt_price_below_min_as_spec_violation`
- `register_v4_pool_rejects_tick_below_min_as_spec_violation`
- `register_v4_pool_rejects_fee_at_v4_max_as_spec_violation` (the test confirms `fee >= 1 << 24` is surfaced as `SpecViolation`, NOT `DynamicFee` — dynamically-flagged fees stay canonical to `DynamicFee`).
- `register_v4_pool_rejects_tick_spacing_out_of_range_as_spec_violation`
- `register_v4_pool_accepts_in_spec_params` (green companion at every validator's accept boundary).

## Adaptive test fixes (silently stale `sqrt_price_x96 = 1`)
The spec validators surfaced that several registry tests had been passing `sqrt_price_x96 = 1` to `register_v3_pool` — a value below `MIN_SQRT_RATIO = 4_295_128_740`, which silently degraded to a malformed pool state previously. This is *exactly* the silent-degradation case the spec-bound admission was designed to surface; ADR-001's "I/O-free pools" contract forbids out-of-range prices at the V3/V4 storage layer, and the Rust core now enforces it. Two test files updated to use the canonical 1:1 sqrt `1 << 96`:

- `tests/registry/test_registration_verify.py` (5 instances: V3 mainnet / PancakeSwap separate-deployer / wrong-address-rejection variants — these exercise CREATE2 verification, not math, so the in-spec value keeps their assertion intent).
- `tests/registry/test_v3_handle_identity.py` (1 instance: keyed-init-hash-off-handle test).

`tests/builders/test_pybot_io.py` also uses `sqrt_price_x96 = 0` for a mock-provider in-disk fetcher test, but it never traverses `register_v3_pool` (the `sqrt_price` is just stored on the mock `_V3PoolProvider` and round-tripped back through `fetch_v3_immutable_data`); untouched.

## Validation
- `cargo test -p degenbot-bot --lib`: 375 (was 369; +6 new), all green.
- `cargo clippy -p degenbot-bot --all-targets`: clean.
- `cargo fmt -p degenbot-bot -p degenbot_rs --check`: clean.
- `just check-no-pyo3-in-cores`: green (the new V4 validator's `SpecViolation` arm lives in `degenbot-python`, the PyO3 wrapper, not in a core crate).
- `cargo build --example standalone_consumer`: clean.
- `cargo test -p degenbot_rs --test python_integration`: 16/16 green.
- `uv run pytest tests/`: 3120 passed, 4 skipped.

## Followups
- **F2EVV6** (blocked-by this task complete; now ready): replace all three stop-gap `PyValueError({e:?})` mappers (V2 / V3 / this V4 `SpecViolation` arm) with the typed `PoolRegistrationError` hierarchy + a shared `SpecViolationError`. The V4 `HookedPool` / `DynamicFee` already have typed exceptions; the spec-violation arms will get a typed `SpecViolationError` mirroring them. Same migration for V2/V3 admissions, with the F2 stop-gap mappers as the migration seam.
- **RNZQUO** (final audit + ADR note): unblocked once F2EVV6 lands. The audit will re-confirm: (a) no `assert!`-style duplicate-check panic remains on any `register_v{2,3,4}_pool`, (b) the typed-error types are surfaced uniformly through `PyValueError`→ typed exceptions at the seam, (c) ADR-001 (I/O-free pools) explicitly references spec-bound admission in the *admission* clause.
