# Issue 0034: MINT_TO_TREASURY Rounding Direction Error

## Date
2026-03-18

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...WETH...). User ... scaled balance (5091802364584999131023) does not match contract balance (5091802364584999131024) at block 23159452
```

**Balance Difference:** 1 wei (calculated: 5091802364584999131023, contract: 5091802364584999131024)

## Root Cause

The `_calculate_mint_to_treasury_scaled_amount()` function used `ray_div` (half-up rounding) to convert the MintedToTreasury event's underlying amount to scaled units. However, the contract uses `rayMulFloor` to calculate the MintedToTreasury amount from the scaled amount:

```solidity
// Pool contract (rev_9.sol:9358)
function getATokenBalance(uint256 scaledAmount, uint256 liquidityIndex) internal pure returns (uint256) {
    return scaledAmount.rayMulFloor(liquidityIndex);
}
```

To reverse this calculation (get scaled from underlying), we need CEIL rounding:
- Contract: `MintedToTreasury.amount = accruedToTreasury.rayMulFloor(index)`
- Reverse: `accruedToTreasury = ray_div_ceil(MintedToTreasury.amount, index)`

### The Math

**WETH Transaction Data:**
- Block: 23159452
- Asset: WETH (0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2)
- aToken: aEthWETH (0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8, Rev 4)
- Treasury: 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c
- MintedToTreasury.amount: 76,116,689,027,312,564,277
- Index: 1,051,094,981,887,882,471,312,148,250

**Incorrect Calculation (half-up):**
```python
ray_div(76116689027312564277, 1051094981887882471312148250) = 72,416,565,904,061,875,430
```

**Correct Calculation (CEIL):**
```python
ray_div_ceil(76116689027312564277, 1051094981887882471312148250) = 72,416,565,904,061,875,431
```

**Contract actual mint:** 72,416,565,904,061,875,431 ✓

## Transaction Details

| Field | Value |
|-------|-------|
| **Transaction Hash** | 0x764eb8aefa979c1a1c1a773514b75543b5ca0dfe6929b8740d1867dd2ee391e5 |
| **Block** | 23159452 |
| **Type** | MINT_TO_TREASURY (51 reserves processed) |
| **From** | 0x3Cbded22F878aFC8d39dCD744d3Fe62086B76193 |
| **To** | 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 (Aave V3 Pool) |
| **User (Treasury)** | 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c |
| **Pool Revision** | 9 |
| **aToken Revision** | 4 |

## Smart Contract Control Flow

```
Pool.mintToTreasury(assets)
  → PoolLogic.executeMintToTreasury()
    → accruedToTreasury = reserve.accruedToTreasury (scaled)
    → amountToMint = accruedToTreasury.rayMulFloor(index)  // underlying
    → IAToken.mintToTreasury(accruedToTreasury, index)  // passes scaled
      → AToken._mintScaled(POOL, treasury, accruedToTreasury, index)
        → _mint(treasury, accruedToTreasury)  // mints scaled tokens
        → emit Mint(caller, treasury, amountToMint, balanceIncrease, index)
    → emit MintedToTreasury(asset, amountToMint)  // underlying
```

### Key Contract Code

**PoolLogic.executeMintToTreasury (rev_9.sol:127-129):**
```solidity
uint256 amountToMint = accruedToTreasury.getATokenBalance(normalizedIncome);  // rayMulFloor
IAToken(reserve.aTokenAddress).mintToTreasury(accruedToTreasury, normalizedIncome);
emit MintedToTreasury(assetAddress, amountToMint);
```

**TokenMath.getATokenBalance (rev_9.sol:9354-9359):**
```solidity
function getATokenBalance(uint256 scaledAmount, uint256 liquidityIndex) internal pure returns (uint256) {
    return scaledAmount.rayMulFloor(liquidityIndex);
}
```

## The Fix

**File:** `src/degenbot/cli/aave.py`

**Change:** Line 2798

```python
# Before (incorrect - uses half-up rounding)
scaled_amount = ray_div(minted_amount, scaled_event.index)

# After (correct - uses CEIL rounding to reverse rayMulFloor)
scaled_amount = ray_div_ceil(minted_amount, scaled_event.index)
```

**Also need to import ray_div_ceil:**

**File:** `src/degenbot/cli/aave.py`

**Change:** Line 31-34

```python
# Before
from degenbot.aave.libraries.wad_ray_math import (
    ray_div,
    wad_mul,
)

# After
from degenbot.aave.libraries.wad_ray_math import (
    ray_div,
    ray_div_ceil,
    wad_mul,
)
```

**Rationale:**
1. Pool calculates `MintedToTreasury.amount = accruedToTreasury.rayMulFloor(index)`
2. To reverse: `accruedToTreasury = ray_div_ceil(MintedToTreasury.amount, index)`
3. CEIL is the mathematical inverse of FLOOR for this operation

## Key Insight

**When reversing ray math operations, use the inverse rounding mode.**

- If contract uses `rayMulFloor`, reverse with `ray_div_ceil`
- If contract uses `rayMulCeil`, reverse with `ray_div_floor`
- If contract uses `rayMul` (half-up), reverse with `ray_div` (half-up)

This is similar to how subtraction reverses addition - the rounding mode must be inverted to maintain mathematical consistency.

## Additional Fix: Database Reset

Before applying the code fix, the treasury position balances needed to be reset to match on-chain state at block 23159451 due to accumulated errors from previous processing. This was done using `scripts/reset_treasury_balances.py`.

## Fix Status

**Proposed:** 2026-03-18
**Implemented:** 2026-03-18
**Tested:** 2026-03-18
**Status:** ✅ Verified - Fix working correctly

### Changes Made

1. **File:** `src/degenbot/cli/aave.py` (line 31-34)
   - Added `ray_div_ceil` to imports

2. **File:** `src/degenbot/cli/aave.py` (line 2798)
   - Changed `ray_div` to `ray_div_ceil` in `_calculate_mint_to_treasury_scaled_amount()`

3. **File:** `src/degenbot/cli/aave.py` (line 2106-2113)
   - Restored exact match assertion (removed tolerance)

### Test Results

```bash
$ uv run degenbot aave update
AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True) successfully updated to block 23,159,452
```

**Verification:**
- Block 23159452 (original failure): ✅ Processed successfully
- All 51 MINT_TO_TREASURY operations processed without errors
- Balance verification passes with exact match (no tolerance)

## Summary

**Issue:** Balance verification failure for MINT_TO_TREASURY operations due to incorrect rounding direction when reversing the Pool contract's rayMulFloor calculation.

**Root Cause:** The code used `ray_div` (half-up rounding) instead of `ray_div_ceil` to convert MintedToTreasury.amount (underlying) back to scaled units.

**Fix:** Changed `_calculate_mint_to_treasury_scaled_amount()` to use `ray_div_ceil()` instead of `ray_div()`, matching the mathematical inverse of the contract's `rayMulFloor` operation.

**Impact:** All MINT_TO_TREASURY operations now calculate exact scaled amounts that match contract state, allowing balance verification to pass with strict equality (no tolerance required).
