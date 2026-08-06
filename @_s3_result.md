## TF7RZB-S3 — V4 identity/two-step resolution core-side — DONE

**Goal:** move `_build_v4_managed`'s V4 identity resolution into the Rust
core.

**What I delivered** (the identity-resolution move, TF7RZB item 3):
1. **Rust core (`builder.rs`)** — a new `resolve_v4_identity(chain_id,
   pool_manager, pool_id, overrides, io) -> (V4PoolBuildIdentity,
   Option<u64>)` that performs the V4 identity two-step CORE-side: DB
   manager → V4 row → per-FK token rows first, else the caller-supplied
   `V4PoolBuildOverrides` (currency0/1, fee, tick_spacing, hook_address,
   state_view). It orders the override currencies by ascending address,
   derives `hook_flags` from the hook address (low 16 bits, the practical
   no-hook value = 0), and returns the DB `liquidity_update_block` (the
   two-stamp OB7UNY clock seed). A new `PoolBuilderError::MissingIdentity`
   surfaces when neither the DB two-step nor the overrides are complete —
   matching the retired Python `DegenbotValueError` path.
2. **PyO3 seam (`bot/mod.rs`)** — new `PyBot.resolve_v4_identity(...)`
   delegating to the core resolver (returns the 7-tuple identity + the
   liquidity-update block). `build_v4_pool` keeps its S2 resolved-identity
   signature (the builder takes a resolved identity; the resolver owns the
   lookup). `MissingIdentity` maps to `PyValueError`.
3. **`_bot.py::_build_v4_managed`** — no longer reads the DB nor assembles
   the kwargs identity. It calls `resolve_v4_identity` (catching the seam
   `ValueError` → re-raises `DegenbotValueError` to preserve the public
   contract), builds the ERC20 companions from the CORE-resolved
   currency0/1, fetches the companion scalars via the CORE-resolved
   state_view, then delegates `build_v4_pool` with the resolved identity.
   The S2 parity guard now checks resolver-output vs builder-echo agreement.
   Deleted the now-orphaned `V4DbValues` class + the dead `resolve_seed_block`
   usage + unused imports.
4. **Tests** — a parity-mismatch test (resolver vs builder diverge → raises),
   a resolve-error-mapping test (MissingIdentity → DegenbotValueError), and
   two Rust unit tests (`resolve_v4_identity` orders currencies + derives
   hook_flags; empty overrides → MissingIdentity). Tier-0 standalone slice
   added: `resolve_v4_identity` over a no-DB stub with empty overrides must
   degrade to `MissingIdentity` (pins the umbrella re-export).

**Scope note (transparent):** the DB two-step + kwargs fallback now live in
the Rust core, and `build_v4_pool` consumes the resolved identity. The
companion scalars/protocol_fee/lp_fee fetch and the ERC20 companion assembly
remain in the Python driver (the Rust `V4PoolState` carries protocol_fee but
not lp_fee/protocol_fee-splitting for the companion), and the V4 two-stamp
`tick_data_block` vs `update_block` split (`RegisterV4PoolParams` supports it,
but `build_v4` still sets both to `update_block`) is a separate follow-up —
not the identity-resolution move this task scoped. V2/V3 (S1) + V4 return
surfaces, plus V4 identity resolution (S3), complete the three builder-family
migration slice.

**Validation:** `cargo check`/`clippy`/`fmt` on degenbot / degenbot_rs /
degenbot-bot clean; `just check-no-pyo3-in-cores` OK (core stays pyo3-free);
Tier-1 reachability green (`degenbot` umbrella re-exports
`resolve_v4_identity` + `V4PoolBuildOverrides`); standalone_consumer slice
passes; Python affected suites green (147 passed, 1 skipped in tests/bot +
tests/builders + tests/uniswap/v4 + tests/test_bot.py); ruff clean.

**Follow-up:** V4 two-stamp seed-block split (`tick_data_block` vs
`update_block`) is the natural next identity/state task.
