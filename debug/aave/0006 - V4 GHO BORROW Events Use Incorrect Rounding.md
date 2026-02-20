# Aave Debug Progress

## Issue: V4 GHO BORROW Events Use Incorrect Rounding

**Date:** 2025-02-19

**Symptom:** 
```
AssertionError: User 0x1EB017aA019b8c4B0e3dCBB1b972767Dd791d47f: debt balance (5087640786937096202415) does not match scaled token contract (5087640786937096202416) @ 0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B at block 23088596
```

**Root Cause:** 
The `GhoV4Processor.process_mint_event()` method in `src/degenbot/aave/processors/debt/gho/v4.py` uses **half-up rounding** (`ray_div`) for GHO BORROW operations, but revision 4+ vToken contracts use **ceiling division** (`ray_div_ceil`) via `TokenMath.getVTokenMintScaledAmount()`.

**The calculation:**
- Borrow amount: 50,000 GHO (50,000,000,000,000,000,000)
- Index: 1,143,506,033,072,253,901,310,332,511
- Exact scaled amount: 43,725,173,767,264,842,764.36...
- Current code (ray_div): 43,725,173,767,264,842,764 (1 wei too low) ❌
- Contract (ray_div_ceil): 43,725,173,767,264,842,765 (correct!) ✅

**Transaction Details:**
- **Hash:** 0xfbec429cc9bc927cce5f700c32b3f7cf15409831042d067bc13cda975d1a2cab
- **Block:** 23088596
- **Type:** GHO BORROW (borrow function on Pool contract)
- **User:** 0x1EB017aA019b8c4B0e3dCBB1b972767Dd791d47f
- **Asset:** vGHO (0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B, revision 5)
- **Events:** 
  - Mint at logIndex 530: value=52,441,931,039,374,678,683, balanceIncrease=2,441,931,039,374,678,683, index=1,143,506,033,072,253,901,310,332,511

**Fix:**
Changed the BORROW case in `process_mint_event()` in `src/degenbot/aave/processors/debt/gho/v4.py` from `ray_div` to `ray_div_ceil`:

```python
if event_data.value > event_data.balance_increase:
    # GHO BORROW: emitted in _mintScaled
    # Revision 4+ uses ceiling division (ray_div_ceil) to match
    # TokenMath.getVTokenMintScaledAmount behavior.
    requested_amount = event_data.value - event_data.balance_increase
    balance_delta = wad_ray_math.ray_div_ceil(
        a=requested_amount,
        b=event_data.index,
    )
    user_operation = GhoUserOperation.GHO_BORROW
```

**Key Insight:**
According to Aave's TokenMath library:
- `getVTokenMintScaledAmount()` uses `rayDivCeil` (ceiling division) to ensure the protocol never underaccounts user's debt
- `getVTokenBurnScaledAmount()` uses `rayDivFloor` (floor division) to prevent over-burning

**Corrected Rounding Strategy Table:**

| Token Type | Operation | Revision 1-3 | Revision 4+ (Contract) | Processor Delta |
|------------|-----------|--------------|------------------------|-----------------|
| **aToken** | Mint (SUPPLY) | Half-up | Floor | Floor |
| **aToken** | Burn (WITHDRAW) | Half-up | Ceiling | Ceiling |
| **vToken** | Mint (BORROW) | Half-up | Ceiling | Ceiling |
| **vToken** | Burn (REPAY) | Half-up | Floor | Ceiling |
| **GHO vToken** | Mint (BORROW) | Half-up | Ceiling | Ceiling |
| **GHO vToken** | Burn (REPAY) | N/A | Floor | Floor |

**Testing:**
- Aave update processes block 23088596 successfully
- GHO debt balances match on-chain values

**Refactoring:**
Consider creating a unified rounding configuration module that defines the rounding mode for each (token_type, operation, revision) combination, making it easier to verify correctness and add support for new revisions.
