# Plan 044: Deprecate Bot's Typed Build Pass-Throughs

> **Note**: The deprecated methods `build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool`, and `get_web3` were deleted by Plan 059.

## Overview

Mark the typed build methods on `Bot` (`build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool`) as deprecated in favor of `build_pool()`. Also review the ERC-20 convenience methods (`get_token_balance`, `get_token_approval`, `get_token_total_supply`, `get_ether_balance`) for possible relocation.

This is a small cleanup that concentrates Bot's interface on its core concerns: Pool building via `build_pool()`, Token building via `build_erc20token()`, type resolution, and state updates.

## Files Involved

**Primary:**
- `src/degenbot/bot.py` — add `DeprecationWarning` to typed build methods; consider moving ERC-20 I/O methods to `Erc20Builder`-exposed methods

**Secondary:**
- `tests/` — replace `bot.build_v2_pool(...)` calls with `bot.build_pool(...)` where they exist
- `docs/` — update any examples that use typed build methods
- `src/degenbot/erc20/erc20.py` — if ERC-20 I/O methods move, add corresponding methods or expose the builder

## Problem

### Deletion test

If you deleted `build_v2_pool()`, callers would use `build_pool()` instead. The complexity doesn't reappear — `build_pool()` already has the full dispatch logic. The typed methods are pure pass-throughs:

```python
def build_v2_pool(self, pool_address, *, chain_id=None, ...) -> UniswapV2Pool:
    return self._v2_builder.build(pool_address, chain_id=chain_id, ...)
```

They're shallow — the interface is nearly as complex as the implementation (both list the same keyword arguments). They provide type narrowing on the return type, which is their only argument for existing.

### Current inventory of pass-through methods

| Method | Delegates to | Return type narrowing | Callers in codebase |
|--------|-------------|----------------------|---------------------|
| `build_v2_pool` | `self._v2_builder.build()` | `UniswapV2Pool` | Minimal (tests mostly use `build_pool()`) |
| `build_v3_pool` | `self._v3_builder.build()` | `UniswapV3Pool` | Minimal |
| `build_v4_pool` | `self._v4_builder.build()` | `UniswapV4Pool` | Minimal |
| `build_curve_pool` | `self._curve_builder.build()` | `CurveStableswapPool` | Minimal |
| `build_erc20token` | `self._erc20_builder.build()` | `Erc20Token` | N/A (this is the primary entry point) |
| `get_token_balance` | `self._erc20_builder.get_token_balance()` | `int` | Internal |
| `get_token_approval` | `self._erc20_builder.get_token_approval()` | `int` | Internal |
| `get_token_total_supply` | `self._erc20_builder.get_token_total_supply()` | `int` | Internal |
| `get_ether_balance` | `self._erc20_builder.get_ether_balance()` | `int` | Internal |
| `update` | `builder.update()` (via registry) | `bool` | N/A (not a pass-through, has real dispatch logic) |

The `build_*_pool()` methods are 4 pass-throughs totaling ~80 lines. The ERC-20 I/O methods are 4 pass-throughs totaling ~30 lines.

### What `build_pool()` already handles

```python
def build_pool(self, address, *, pool_id=None, chain_id=None, ...):
    # V4 fast path (pool_id discriminates V4 managed pools)
    if pool_id is not None:
        return self.build_v4_pool(pool_id=pool_id, ...)
    
    # Check pool registry — return existing pool if already built
    existing = self.pools.get(...)
    if existing is not None:
        return existing
    
    # Resolve pool type from DB, registry, or on-chain probing
    pool_type = self._resolve_pool_type(...)
    
    # Dispatch to builder via registry
    pool_class = self._pool_class_for_descriptor(...)
    builder = self._builders.get(pool_class) or MRO fallback
    
    return builder.build(address, chain_id=chain_id, ...)
```

`build_pool()` does everything. The typed methods just pre-select the builder, which `build_pool()` does automatically via type resolution.

## Solution

### Step 1: Add `DeprecationWarning` to typed build methods

```python
def build_v2_pool(self, pool_address, *, chain_id=None, ...) -> UniswapV2Pool:
    """.. deprecated:: 0.x
        Use ``build_pool(address)`` instead. Type resolution automatically
        selects the correct builder.
    """
    warnings.warn(
        "build_v2_pool() is deprecated — use build_pool(address) instead.",
        DeprecationWarning,
        stacklevel=2,
    )
    return self._v2_builder.build(pool_address, chain_id=chain_id, ...)
```

Same for `build_v3_pool`, `build_v4_pool`, `build_curve_pool`.

### Step 2: Update `get_web3` deprecation consistency

`get_web3()` already has a deprecation warning. Ensure the new typed-build deprecation messages are consistent in style.

### Step 3: Review ERC-20 I/O methods

The four ERC-20 I/O methods (`get_token_balance`, `get_token_approval`, `get_token_total_supply`, `get_ether_balance`) are pass-throughs, but they serve a different purpose than the build methods: they provide I/O operations on constructed tokens, not construction. They belong on Bot because Bot is the I/O boundary.

**Decision: keep them on Bot for now.** They're a smaller concern (4 short methods vs. 4 long ones), and moving them would require callers to hold a reference to the ERC-20 builder. A future plan can relocate them if needed.

### Step 4: Remove typed build methods (after deprecation period)

After one release cycle with deprecation warnings, remove the four methods. All callers should have migrated to `build_pool()`.

## Status: Complete

### Changes Made

- **Deprecation warnings added** to `Bot.build_v2_pool()`, `Bot.build_v3_pool()`, `Bot.build_v4_pool()`, `Bot.build_curve_pool()` — all emit `DeprecationWarning` with `stacklevel=2` directing users to `build_pool()`
- **`Bot.build_pool()` refactored** to call builders directly (not the deprecated typed methods), avoiding recursive deprecation warnings
- **`AsyncBot` typed build methods** also received deprecation warnings
- **V4 kwargs forwarded** through `build_pool()`: `state_view_address`, `tokens`, `fee`, `tick_spacing`, `hook_address` added as optional kwargs
- **~85 test calls updated** from `bot.build_v*_pool()` / `bot.build_curve_pool()` to `bot.build_pool()`
- **Error message fixed** in `_resolve_pool_type()`: removed "Use build_v2_pool, build_v3_pool..." suggestion
- **Comment references updated** across codebase (src and tests)
- **AsyncBot tests** keep calling deprecated methods (AsyncBot lacks `build_pool()`) with `@pytest.mark.filterwarnings("ignore::DeprecationWarning")`
- **All non-fork tests pass** (1233+ green)

### Not Changed (Per Plan)

- **ERC-20 I/O methods** (`get_token_balance`, `get_token_approval`, `get_token_total_supply`, `get_ether_balance`) kept on Bot — they serve as I/O boundary, not build pass-throughs
- **`build_erc20token()`** kept — it's the primary entry point for tokens
- **`build_v*_pool()` methods not removed yet** — per plan, removal waits one release cycle after deprecation

### Line Count

- `bot.py`: 766 lines (typed methods still present but deprecated)
- After removal in next release: ~686 lines (~80 lines removed)

1. **Add `DeprecationWarning`** to `build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool` (they remain functional)
2. **Search codebase** for direct calls to these methods and update to `build_pool()`
3. **Update documentation** to recommend `build_pool()` exclusively
4. **Wait one release cycle** (or until CI confirms zero calls to deprecated methods)
5. **Remove the four methods** from Bot
6. **Remove the `get_web3` deprecation** and the method together (already deprecated)

## Testing

### Immediate (steps 1–2)

All tests pass with deprecation warnings. Tests that call typed build methods are updated to use `build_pool()`.

### After removal (step 5)

All tests pass without the typed methods. `build_pool()` handles all pool construction.

## Benefits

- **Bot's interface concentrates on its core concerns:** `build_pool()`, `build_erc20token()`, `update()`, type resolution, registry access
- **No shallow pass-throughs:** every method on Bot is either doing real work or delegating to a different concern (ERC-20 I/O)
- **~80 lines removed** from Bot after the methods are deleted
- **Consistent API:** all pool construction goes through one method (`build_pool()`), reducing confusion for new contributors

## Risks

- **Return type narrowing lost:** `build_pool()` returns `AbstractLiquidityPool`, while `build_v2_pool()` returns `UniswapV2Pool`. Callers that rely on the narrower return type need an explicit cast or type: ignore. This is a minor inconvenience — most callers don't need the narrowing because they use the pool through its protocol interface (`ConstantProductPool`, `ArbitragePathPool`, etc.).
- **External callers:** library users who call `bot.build_v2_pool()` will see deprecation warnings. This is intentional — the migration path is clear (`build_pool()`).

## Relationship to Other Plans

- **Plan 028** (Builder Registry): Complete. The builder registry makes `build_pool()` sufficient — it dispatches automatically. The typed methods are now redundant.
- **Plan 006** (Universal build_pool): Complete. `build_pool()` was designed to replace the typed methods.
- **Plan 043** (Extract V2 Variant Builders): This plan is complementary. After 043, the typed V2 build method would no longer handle Aerodrome or Camelot — another reason to deprecate it in favor of `build_pool()`.
