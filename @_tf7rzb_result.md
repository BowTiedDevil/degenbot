# TF7RZB — Sub-A2: Rust PoolBuilder return-surface + V4 identity resolution core-side

## Summary
A genuine "move-to-Rust" step: the Python registration driver no longer has to
re-derive pool identity, and the remaining V4 identity/two-step resolution moved
into the Rust core. Delivered across three ordered sub-tasks (S1→S3), each landed
and committed separately.

## S1 — V2/V3 builder identity return surface (`66e278b1`)
`PyBot.build_v2_pool` / `build_v3_pool` now return a small typed identity struct
(pool_id, token0/1, family, address) instead of only a `pool_id` int, so the
Python driver consumes identity directly instead of re-deriving it from a
`PyLiquidityPool` for `resolve_directions` / `register_path`.

## S2 — V4 builder identity return surface (`0020a671`)
`build_v4_pool` returns `(pool_id, pool_id_hex, ...)` identity, and the
`V4PoolBuildIdentity` / `V4PoolBuildOverrides` core structs are introduced to
carry the resolved V4 identity across the FFI seam.

## S3 — V4 identity/two-step resolution core-side (`5420f69d`)
`_build_v4_managed`'s V4 identity resolution (DB two-step + caller-kwargs
fallback + spec-bounds admission surfacing) moved into the Rust core:
`builder::resolve_v4_identity` performs manager → V4 row → per-FK-token two-step
first, else caller-supplied overrides (currency0/1, fee, tick_spacing,
hook_address, state_view), ordering currencies and deriving hook_flags; a typed
`MissingIdentity` surfaces when neither is complete. `PyBot.resolve_v4_identity`
delegates to it; `build_v4_pool` keeps the S2 resolved-identity signature.
`_build_v4_managed` no longer reads the DB or assembles kwargs identity — it
calls the seam, builds ERC20 companions from the core-resolved currency0/1, and
re-raises `MissingIdentity` as `DegenbotValueError`.

## Acceptance criteria — met
- **Facade-free registration:** a facade-free `_consume_step` can drive V2/V3/V4
  registration with NO Python logic re-implemented — the builder returns identity
  and V4 is built from `(PoolManager, pool_id)` alone (the core `resolve_v4_identity`
  does the DB→kwargs→spec-bounds resolution).
- **V4 identity/two-step enforced core-side** matching today's
  `build_managed_pool` outcomes byte-for-byte, with admission refusals
  (`HookedPoolRejectedError` / `DynamicFeePoolRejectedError`) preserved via the
  `MissingIdentity` / admissions surface.
- **Standalone reachability + Tier-1/Tier-2 parity green** for the widened
  surface (umbrella re-exports `resolve_v4_identity` + `V4PoolBuildOverrides`;
  parity test checks resolver-vs-builder agreement).

## Validation gates — green
- `just test-standalone` (Tier-0 standalone consumer reaches the builder return surface).
- `cargo test -p degenbot-bot --lib pool_builder` → 48 passed (incl.
  `resolve_v4_identity_orders_currencies_and_derives_hook_flags`,
  `resolve_v4_identity_empty_overrides_is_missing_identity`, resolve-error
  mapping).
- `cargo test -p degenbot --test reachability` → 2 passed (Tier-1).
- `uv run pytest tests/test_bot.py` + V4/V3 shared-state registry → 23 passed.
- `cargo clippy -p degenbot-bot --lib -D warnings` + `cargo clippy -p degenbot_rs --lib -D warnings` → clean.

## Deferred (as scoped)
Wiring into the background registration task (Sub-B/Sub-C, already landed) and
the add-at-any-time end-to-end (NWTUM3, already landed) — both built on this
surface.
