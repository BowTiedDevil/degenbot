# F2EVV6 — PyO3 typed-error mappers + Python `register_v2/v3` bindings

## Outcome

Promoted the WOYYS2 stop-gap `PyValueError("V× pool registration failed: {e:?}")` mappers (MSTAT2 for V2, 24KNGF for V3, K3IICB for V4) to a **typed `PoolRegistrationError` Python exception hierarchy**, shared uniformly across V2/V3/V4. The hierarchy lives on the Rust side as `create_exception!` pyclasses in `rust/crates/degenbot-python/src/bot/engine/errors.rs`, mirroring the existing Plan-102 V4 precedent (`HookedPoolRejectedError` / `DynamicFeePoolRejectedError`) and registered in `c_api.rs` + exposed in `src/degenbot/degenbot_rs.pyi`.

Hierarchy:

```
ValueError
└─ PoolRegistrationError                       (F2EVV6 base)
   ├─ HookedPoolRejectedError                    (V4 admission —
   │                                              amount-modifying hook)
   ├─ DynamicFeePoolRejectedError                (V4 admission — dynamic fee)
   ├─ PoolAlreadyRegisteredError                (V2/V3/V4 — duplicate
   │                                              address at registration)
   └─ SpecViolationError                        (V2/V3/V4 — out-of-spec
                                                  field: sqrt/tick/fee/
                                                  tickSpacing/reserve)
```

Both new classes + the reparented V4 names subclass `PoolRegistrationError`, which subclasses `ValueError`, so a broad `except ValueError:` (used by `build_paths` to skip one rejected pool at a time) keeps working; callers that want to scope just admission refusals can use `except PoolRegistrationError:`.

## Mapper work

- **`map_register_v2_err` (new)** — `RegisterV2PoolError::{AlreadyRegistered → PoolAlreadyRegisteredError, SpecViolation → SpecViolationError}`. Message format: `"V2 pool already registered: address={addr}"` / `"V2 pool registration failed: {v}"` (where `{v}` is the `SpecViolation`'s `Display`: `field <name> value <val> out of bounds: <bound>`).
- **`map_register_v3_err` (new)** — V3 twin of the V2 mapper, mirroring its variant set.
- **`map_register_v4_err` (extended)** — V4's `AlreadyRegistered { pool_manager, pool_id }` arm was promoted from plain `PyValueError` to `PoolAlreadyRegisteredError` (was previously the "plain `ValueError` — duplicate registration is a wiring error, not an admission category" pattern; now unified with the V2/V3 admission hierarchy). The `SpecViolation` arm replaces the K3IICB stop-gap `PyValueError("V4 pool registration failed: spec violation — {v}")` with the typed `SpecViolationError`. The V4-specific `HookedPool` / `DynamicFee` arms keep emitting their typed `HookedPoolRejectedError` / `DynamicFeePoolRejectedError` (the patient message text is byte-identical so existing Python classifications by `isinstance` continue to match).

## PyO3 wrappers

- `PyBot::register_v2_pool` — stop-gap mapper replaced with `.map_err(map_register_v2_err)` (import added alongside `map_register_v4_err` at `bot/mod.rs:30`).
- `PyBot::register_v3_pool` — same, with `map_register_v3_err`.
- `PyBot::register_v4_pool` — unchanged at the call site (was already using `.map_err(map_register_v4_err)` as a tail expression); the upgrade is inside `map_register_v4_err`.

`engine/mod.rs` re-exports all three mappers as `pub(crate) use register::{map_register_v2_err, map_register_v3_err, map_register_v4_err};` (was only `map_register_v4_err`).

## Standalone-Rust-core constraint

All new `#[pyclass]` exception types are defined via `create_exception!` in `rust/crates/degenbot-python/src/bot/engine/errors.rs` — i.e. **in the PyO3 wrapper, not in any core crate** (enforced by `just check-no-pyo3-in-cores`). A standalone Rust consumer building `cargo add degenbot` continues to receive `RegisterV×PoolError` from the bot core; the Python exception hierarchy is purely a translation layer at the PyO3 boundary per ADR-005's "PyO3 wrapper = arg extract → GIL release → core call → result wrap" rule. No business logic in the wrappers; the mappers are pure `RegisterV×PoolError → PyErr` translation tables.

## Red → Green

New Python test file `tests/rust/test_pybot_admission_exceptions.py` — 19 tests through the actual `PyBot.register_v{2,3,4}_pool` Python seam:

- 4 hierarchy-shape tests (exported, parented under `PoolRegistrationError`, backward-compat with `except ValueError:` net, reparenting of the V4 names).
- 5 V2 seam tests: duplicate address (`PoolAlreadyRegisteredError`), 1 reserve spec violation (`SpecViolationError("reserve0 > uint112")`).
- 5 V3 seam tests: duplicate address, sqrt_price below min, sqrt_price at MAX (`SpecViolationError("sqrtPriceX96")`), tick below MIN, tick_spacing above MAX.
- 5 V4 seam tests: duplicate address, sqrt_price out-of-spec, V4 fee above `1 << 24` (`SpecViolationError`, NOT `DynamicFeePoolRejectedError`), confirming the spec validators run before the hook/dynamic-fee rejections so a hooked pool reaches the `HookedPoolRejectedError` arm intact (since sqrt/tick/fee/spacing are all in-spec in the hooked test).

Extended existing `tests/rust/test_v4_admission_exceptions.py` docstring + a new `test_v4_admission_errors_are_pool_registration_errors` test (asserts the reparented V4 variants subclass `PoolRegistrationError`). Also fixed a pre-existing `raw-string-in-exception` ruff finding in that file (`raise exc_type("rejected")` → variable-assignment form).

`degenbot_rs.pyi` updated: new class-defs for `PoolRegistrationError`, `PoolAlreadyRegisteredError`, `SpecViolationError`; reparented `HookedPoolRejectedError` / `DynamicFeePoolRejectedError` under `PoolRegistrationError` (were directly under `ValueError`).

## Coordination

t-5092 is mid-C3 (AnvilFork port, parked on a user decision; not touching any Rust). The Python registration-exception classes introduced by F2EVV6 are defined on the Rust side via `create_exception!` (matching the V4 precedent) — **nothing was added to `degenbot/exceptions/__init__.py`**, sidestepping the t-5092 coordination flag on its RUF022-sorted `__all__`. The new exception types' Python presence is exposed only via `c_api.rs` registration + the `degenbot_rs.pyi` stub (the same surface as the existing V4 variants).

## Validation

- `cargo test -p degenbot-bot --lib`: 375 passed, 0 failed (no Rust-core regression — the mapper layer is PyO3-only).
- `cargo test -p degenbot_rs --test python_integration`: 16/16 green.
- `cargo clippy -p degenbot-bot -p degenbot_rs --all-targets`: clean.
- `cargo fmt -p degenbot-bot -p degenbot_rs --check`: clean.
- `cargo build -p degenbot --example standalone_consumer`: clean.
- `just check-no-pyo3-in-cores`: green (no `pyo3` in any core crate; all `create_exception!` calls live in `degenbot-python`).
- `uv run pytest tests/`: 3140 passed, 4 skipped (was 3120 baseline pre-F2EVV6; +20 new tests).
- `uv run ruff check` + `uv run ruff format --check` over the touched Python files: clean.

## Followups

- **RNZQUO (final audit + ADR note)** — was blocked-by F2EVV6; now unblocked. The audit will re-confirm: (a) no `assert!`-style duplicate-check panic remains on any `register_v{2,3,4}_pool`; (b) the typed-error types surface uniformly through `PyValueError` → typed exceptions at the PyO3 seam; (c) ADR-001 (I/O-free pools) explicitly references spec-bound admission in the admission clause; (d) optionally add a note that the `.pyi`'s `class HookedPoolRejectedError(PoolRegistrationError)` was a direct reparent in F2EVV6 (was previously a direct child of `ValueError`).
- The `register_v4_pool` docstring in `bot/mod.rs` still says "the admission floor lives in `BotState::register_v4_pool` (ADR-005 slice 9a): pools with amount-modifying hooks (`hook_flags & 0xCC != 0`) or dynamic fees (`fee == 0x100000`) are rejected here, surfacing as typed Python exceptions (`HookedPoolRejectedError` / `DynamicFeePoolRejectedError`)". An optional polish: mention that F2EVV6 made ALL admission refusals (including spec violations and duplicate-name rejection) surface as typed `PoolRegistrationError` subclasses. Leaving for RNZQUO's audit pass.
