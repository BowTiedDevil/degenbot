## TF7RZB-S1 — V2/V3 builder identity return surface — DONE

**What I delivered** (the builder return-surface move listed as TF7RZB item 1):
1. **`PyBot.build_v2_pool` / `build_v3_pool` now return a typed identity tuple**
   `(pool_id, token0, token1, address, family)` sourced from the Rust builder's
   already-computed params, instead of a bare `pool_id: int`. The driver now
   consumes core-computed identity and never re-derives token0/1/address/family.
   - V2 `family` = `params.variant.as_str()` (`DexVariant` kebab, e.g.
     `"uniswap-v2"`).
   - V3 `family` = `degenbot_uniswap::deployments::resolve_dex_name(
     chain_id, params.factory)` (Rust-owned single source, e.g. `"uniswap"`),
     falling back to `"uniswap-v3"`.
2. **`_bot.py` `_build_delegated` unpacks the tuple** at both call sites
   (V3 branch + V2 else branch) and adds a **return-surface parity guard**:
   the builder-returned token0/token1 must equal what the registered handle
   exposes; a divergence raises `DegenbotValueError` (a genuine core/driver
   seam bug must fail loudly, not silently re-derive).
3. **`.pyi`** updated to `tuple[int, str, str, str, str]`.
4. **Tests**: new `TestBuildDelegatedIdentityReturnSurface` (parity-mismatch
   raises) in `tests/test_bot.py`; existing builder identity tests
   (`build_v2_assembles_register_params_from_onchain`, 46 pool_builder tests)
   already prove core identity coherence.

**AC mapping:** ✅ builder returns identity (V2+V3); ✅ only 2 Python callers
unpack + `.pyi` updated; ✅ Tier-1 reachability preserved (builder + 
`resolve_dex_name` still reached via umbrella); ✅ parity guard/parity test.

**Validation:** `cargo check -p degenbot_rs` OK; `pool_builder` 46 Rust tests
pass; `reachability` 2 pass; Python affected suites (tests/bot, tests/builders,
tests/test_bot.py, v3 io-free) green; ruff clean on changed files.

**Scope note (transparent):** S1 is the return surface only. The full
facade-free `_consume_step` rewire is NWTUM3; V4 identity/two-step resolution
core-side is S2 (return surface) + S3 (resolution move).

**Follow-up:** S2 (GKS2MO, V4 identity return), S3 (O5FHLF, V4 resolution).
