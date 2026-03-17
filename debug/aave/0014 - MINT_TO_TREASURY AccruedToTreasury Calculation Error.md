# MINT_TO_TREASURY AccruedToTreasury Calculation Error

## Issue

Balance verification failure for Aave Treasury when processing MINT_TO_TREASURY operations. The calculated scaled balance was consistently off by 1 wei from the on-chain contract balance.

## Context

- **Block:** 23109338
- **Transaction:** 0xe921b7eea5cb014e6253835f0929c41123e424947c979b70e32aae164a4551e2
- **Operation:** `mintToTreasury` with 50 different assets
- **Asset Example (WETH):**
  - aToken: 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8
  - User (Treasury): 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c

## Error

```
AssertionError: Balance verification failure for AaveV3Asset(...). 
User AaveV3User(...) scaled balance (4946209943729441654461) 
does not match contract balance (4946209943729441654462) at block 23109338
```

Difference: **1 wei**

## Root Cause

The MINT_TO_TREASURY calculation was using a simplified formula that didn't account for the mathematical properties of floor division when working with existing balances.

### The Problem

The original code calculated:

```python
principal = amount - balance_increase
scaled_amount = ray_div_ceil(principal, index)
```

This assumed that `amount - balance_increase` equals `rayMulFloor(accruedToTreasury, index)`. However, this is mathematically incorrect due to floor division properties.

### Why It Failed

In the AToken contract's `_mintScaled` function (rev_4.sol:2782-2805):

```solidity
function _mintScaled(...) internal returns (bool) {
    uint256 scaledBalance = super.balanceOf(onBehalfOf);
    uint256 nextBalance = getTokenBalance(amountScaled + scaledBalance, index);
    uint256 previousBalance = getTokenBalance(scaledBalance, _userState[onBehalfOf].additionalData);
    uint256 balanceIncrease = getTokenBalance(scaledBalance, index) - previousBalance;
    
    _mint(onBehalfOf, amountScaled.toUint120());
    
    uint256 amountToMint = nextBalance - previousBalance;
    emit Mint(caller, onBehalfOf, amountToMint, balanceIncrease, index);
}
```

Where:
- `getTokenBalance(x, y) = rayMulFloor(x, y)`
- `amountToMint = rayMulFloor(accruedToTreasury + scaledBalance, index) - rayMulFloor(scaledBalance, previousIndex)`
- `balanceIncrease = rayMulFloor(scaledBalance, index) - rayMulFloor(scaledBalance, previousIndex)`

**Key insight:** The Mint event's `amount` field (`amountToMint`) and `balanceIncrease` are calculated using different formulas that involve both the current and previous indices. Simply subtracting `balanceIncrease` from `amount` does not isolate `rayMulFloor(accruedToTreasury, index)`.

## Solution

Recalculate using the exact contract formula with position data:

```python
# Contract formula:
#   nextBalance = rayMulFloor(accruedToTreasury + scaledBalance, index)
#   previousBalance = rayMulFloor(scaledBalance, previousIndex)
#   amountToMint = nextBalance - previousBalance
#
# Solving for accruedToTreasury:
#   nextBalance = amountToMint + previousBalance
#   X = rayDivCeil(nextBalance, index) where X = accruedToTreasury + scaledBalance
#   accruedToTreasury = X - scaledBalance

previous_balance = ray_mul_floor(
    collateral_position.balance,
    collateral_position.last_index or 0,
)
next_balance = scaled_event.amount + previous_balance
X = ray_div_ceil(next_balance, scaled_event.index)
scaled_amount = X - collateral_position.balance
```

This formula:
1. Calculates the treasury's previous balance in underlying units using their stored index
2. Determines the next balance (which equals `amountToMint + previousBalance`)
3. Solves for `X = accruedToTreasury + scaledBalance` using ceiling division
4. Extracts `accruedToTreasury` by subtracting the known scaled balance

## Files Changed

### 1. `src/degenbot/cli/aave.py`

**Import change (line 32):**
```python
# Before:
from degenbot.aave.libraries.wad_ray_math import wad_mul

# After:
from degenbot.aave.libraries.wad_ray_math import ray_div_ceil, ray_mul_floor, wad_mul
```

**Logic change (lines 2666-2686):**
```python
# Before:
# For MINT_TO_TREASURY, the enriched scaled_amount from enrichment.py is already
# calculated correctly using TokenMath (ray_div_floor for V4+). The enrichment layer
# handles the (amount - balance_increase) / index calculation properly.
# Do NOT recalculate here - use the enriched value directly.

# After:
# For MINT_TO_TREASURY, recalculate using the exact contract formula with position data.
# The simple (amount - balance_increase) / index formula is incorrect because amount
# includes interest on the existing balance calculated using the previous index.
#
# Contract formula:
#   nextBalance = rayMulFloor(accruedToTreasury + scaledBalance, index)
#   previousBalance = rayMulFloor(scaledBalance, previousIndex)
#   amountToMint = nextBalance - previousBalance
#
# Solving for accruedToTreasury:
#   nextBalance = amountToMint + previousBalance
#   X = rayDivCeil(nextBalance, index) where X = accruedToTreasury + scaledBalance
#   accruedToTreasury = X - scaledBalance
if operation.operation_type.name == "MINT_TO_TREASURY":
    previous_balance = ray_mul_floor(
        collateral_position.balance,
        collateral_position.last_index or 0,
    )
    next_balance = scaled_event.amount + previous_balance
    X = ray_div_ceil(next_balance, scaled_event.index)
    scaled_amount = X - collateral_position.balance
```

### 2. `src/degenbot/aave/enrichment.py`

**Updated MINT_TO_TREASURY handling (lines 96-129):**
- Changed from using raw `amount` to using `amount - balance_increase`
- Uses `ray_div_ceil` for the calculation
- Added detailed comments explaining the contract behavior

### 3. `src/degenbot/aave/models.py`

**Updated validation (lines 182-188):**
- Modified to skip validation for MINT_TO_TREASURY on pool revision 9+
- The scaled amount is calculated later in aave.py with position data, so validation is not possible at the model level
- The validation is implicitly performed during balance verification

## Verification

### Before Fix
```
WETH:
- Our calculation: 71,492,657,573,754,044,445
- Expected:       71,484,367,636,514,015,634
- Difference:     ~8.29 billion wei

USDC:
- Our calculation: 131,208,912,640
- Expected:       131,208,912,639
- Difference:     1 wei
```

### After Fix
```
All 50 assets in mintToTreasury transaction:
- Calculated balance matches contract balance exactly
- No balance verification errors
```

## Key Insights

1. **Floor division is not reversible:** If `a = floor(b * c / RAY)`, then `b` is not necessarily equal to `ceil(a * RAY / c)` in all cases. The exact value depends on the remainder.

2. **Position data is essential:** For operations like MINT_TO_TREASURY that modify existing balances, you cannot calculate the delta solely from event data. The user's current balance and stored index are required.

3. **Contract arithmetic is exact:** All Solidity math is integer-based with explicit rounding. Python implementations must match exactly, including accounting for how floor division propagates through calculations.

## References

- Pool contract: `contract_reference/aave/Pool/rev_9.sol:109-134`
- AToken contract: `contract_reference/aave/AToken/rev_4.sol:2782-2805`
- TokenMath: `src/degenbot/aave/libraries/token_math.py`
- WadRayMath: `src/degenbot/aave/libraries/wad_ray_math.py`
