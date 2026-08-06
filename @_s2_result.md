## TF7RZB-S2 — V4 builder identity return surface — DONE

**What I delivered** (the V4 return-surface move, TF7RZB item 2):
1. **`PyBot.build_v4_pool` now returns a typed identity tuple**
   `(pool_id, coverage, currency0, currency1, pool_manager, fee,
   tick_spacing, hook_flags, pool_id_hex)` instead of `(pool_id, coverage)`.
   The identity is the normalized caller-supplied surface echoed back through
   the seam (V4 identity is caller-supplied — the core never reads getToken0 /
   getHooks), with checksummed currency/manager and the `[u8;32]` pool_id as
   0x hex.
2. **`_bot.py::_build_v4_managed` unpacks the widened tuple** and adds a
   **return-surface parity guard**: currency0/1, pool_manager, fee,
   tick_spacing, hook_flags, pool_id_hex must round-trip the caller-resolved
   identity losslessly; a divergence raises `DegenbotValueError` (fail loudly,
   never silently re-derive). `coverage` still drives
   `_sparse_liquidity_map`.
3. **`.pyi`** updated to the 9-tuple.
4. **Tests**: new `TestBuildManagedPoolIdentityReturnSurface` (parity-mismatch
   raises) in `tests/test_bot.py`; the existing V4 integration suites
   (`test_v4_pool_io_free`, `test_uniswap_v4_liquidity_pool`,
   `test_uniswap_v4_onchain_parity`, `test_bot_build_managed_pool`) exercise
   the real `build_managed_pool` → `_build_v4_managed` path end-to-end through
   the Rust builder.

**AC mapping:** ✅ build_v4_pool returns identity incl. coverage; ✅ `_bot.py`
unpacks (only caller) + `.pyi` updated; ✅ parity guard/parity test.

**Validation:** `cargo check`/`clippy`/`fmt` on degenbot_rs clean; Python
affected suites (tests/bot, tests/builders, tests/test_bot.py, tests/uniswap/
v4) green; ruff clean on changed files.

**Scope note (transparent):** S2 widens the return surface only. The genuine
state-move (V4 identity two-step DB / kwargs resolution relocating into the
Rust core, per the facade-removal thread) is S3 (O5FHLF). S1 (V2/V3) + S2 (V4)
together give all three builder families a uniform identity return surface.

**Follow-up:** S3 (O5FHLF, V4 identity resolution core-side).
