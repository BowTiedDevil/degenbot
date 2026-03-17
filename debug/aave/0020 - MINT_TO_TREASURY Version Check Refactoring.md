# MINT_TO_TREASURY Version Check Refactoring

## Issue

The `_calculate_mint_to_treasury_scaled_amount` function had accumulated technical debt through iterative bug fixes (Issues 0014, 0015, 0017, 0019). The function was using pool revision checks, but the actual calculation behavior depends on the AToken revision. This refactoring switches to using AToken revision and properly handles Rev 9+ where MintedToTreasury amount is pre-scaled.

## Date

2026-03-17

## Root Cause

### The Problem

**Original Code Issues:**

1. **Line 2691**: `if pool_revision == 8 and minted_to_treasury_amount is not None`
   - **Problem**: MintedToTreasury event exists in ALL revisions (1-10)
   - **Issue**: Rev 9+ also passes scaled amounts and should use this optimization
   - **Misleading**: Comment says "In Rev 8" but the event exists everywhere

2. **Line 2715**: `if pool_revision <= 3`
   - **Problem**: Uses pool revision instead of AToken revision
   - **Reality**: The formula difference is based on AToken rounding behavior:
     - AToken Rev 1-3: Uses standard half-up rounding
     - AToken Rev 4+: Uses TokenMath with floor/ceil rounding

3. **Missing**: No handling for Pool Rev 9+ where amount is already scaled

### Contract Behavior Matrix

| Pool Rev | AToken Rev | Pool.executeMintToTreasury | AToken._mintScaled | Formula Needed |
|----------|------------|---------------------------|-------------------|----------------|
| 1 | 1 | `amountToMint = accruedToTreasury.rayMul(index)` | `amount.rayDiv(index)` (half-up) | Simple, half-up |
| 2 | 2 | Same as Rev 1 | Same as Rev 1 | Simple, half-up |
| 3 | 3 | Same as Rev 1 | Same as Rev 1 | Simple, half-up |
| 4 | 4 | Same as Rev 1 | TokenMath (floor/ceil) | Complex, floor/ceil |
| 5 | 4 | Same as Rev 1 | TokenMath (floor/ceil) | Complex, floor/ceil |
| 6 | 4 | Same as Rev 1 | TokenMath (floor/ceil) | Complex, floor/ceil |
| 7 | 4 | Same as Rev 1 | TokenMath (floor/ceil) | Complex, floor/ceil |
| 8 | 5 | Same as Rev 1 | TokenMath (floor/ceil) | Complex, floor/ceil |
| 9 | 5 | `accruedToTreasury.getATokenBalance(index)` | TokenMath (floor/ceil) | Use pre-scaled amount |
| 10 | 5 | Same as Rev 9 | TokenMath (floor/ceil) | Use pre-scaled amount |

### Key Insight

**The calculation behavior depends on AToken revision, not Pool revision:**

- **AToken Rev 1-3**: Simple formula with half-up rounding
- **AToken Rev 4+**: Complex formula with floor/ceil rounding (or use pre-scaled for Rev 9+)

The Pool revision only affects:
1. Whether `amountToMint` or `accruedToTreasury` is passed to AToken
2. Whether MintedToTreasury amount is pre-scaled (Rev 9+)

## Fix

**File:** `src/degenbot/cli/aave.py`

### 1. Update Function Signature (Line 2648)

Changed parameter from `pool_revision` to `a_token_revision`:

```python
# Before:
def _calculate_mint_to_treasury_scaled_amount(
    ...
    pool_revision: int,
    ...
)

# After:
def _calculate_mint_to_treasury_scaled_amount(
    ...
    a_token_revision: int,
    ...
)
```

### 2. Refactor Version Logic (Lines 2675-2743)

**Before:**
```python
# Multiple confusing checks with misleading comments
if pool_revision == 8 and minted_to_treasury_amount is not None:
    # Comment incorrectly says "In Rev 8"
    return minted_to_treasury_amount

if pool_revision <= 3:
    # Simple formula
    ...
else:
    # Complex formula
    ...
```

**After:**
```python
# 1. Handle BalanceTransfer (applies to all revisions)
if balance_transfer_events:
    ...

# 2. Use pre-scaled amount for Rev 9+ (AToken Rev 5+)
if a_token_revision >= 5 and minted_to_treasury_amount is not None:
    return minted_to_treasury_amount

# 3. Calculate based on AToken revision
if a_token_revision <= 3:
    # Simple formula with half-up rounding
    ...
else:
    # Complex formula with floor/ceil rounding
    ...
```

### 3. Update Call Site (Line 2790)

```python
# Before:
pool_revision=tx_context.pool_revision,

# After:
a_token_revision=collateral_asset.a_token_revision,
```

### 4. Updated Documentation

Updated docstring to clarify:
- The function uses AToken revision, not Pool revision
- Why Rev 9+ can use the MintedToTreasury amount directly
- The relationship between AToken revision and rounding behavior

## Why AToken Revision Matters

The MINT_TO_TREASURY calculation formula must match the AToken contract's implementation:

### AToken Rev 1-3

**Contract Code:** `contract_reference/aave/AToken/rev_1.sol:2762`
```solidity
function _mintScaled(...) internal returns (bool) {
    uint256 amountScaled = amount.rayDiv(index);  // Half-up rounding
    ...
}
```

**Formula:** Simple calculation with half-up rounding
- `balance_increase = ray_mul(balance, index) - ray_mul(balance, last_index)`
- `principal = Mint.amount - balance_increase`
- `scaled_amount = ray_div(principal, index)`

### AToken Rev 4+

**Contract Code:** `contract_reference/aave/AToken/rev_4.sol`
```solidity
function _mintScaled(...) internal returns (bool) {
    // Uses TokenMath with floor rounding
    uint256 nextBalance = getTokenBalance(amountScaled + scaledBalance, index);
    ...
}
```

**Formula:** Complex calculation to reverse floor rounding
- `previous_balance = ray_mul_floor(balance, last_index)`
- `next_balance = Mint.amount + previous_balance`
- `x = ray_div_ceil(next_balance, index)`
- `scaled_amount = x - balance`

## Rev 9+ Optimization

Pool Rev 9 changed the interface:

**Before (Rev 1-8):**
```solidity
uint256 amountToMint = accruedToTreasury.rayMul(normalizedIncome);
IAToken(reserve.aTokenAddress).mintToTreasury(amountToMint, normalizedIncome);
// AToken calculates: scaledAmount = amountToMint.rayDiv(index)
```

**After (Rev 9+):**
```solidity
uint256 amountToMint = accruedToTreasury.getATokenBalance(normalizedIncome);
IAToken(reserve.aTokenAddress).mintToTreasury(accruedToTreasury, normalizedIncome);
// AToken receives pre-scaled amount directly
```

For Rev 9+, `minted_to_treasury_amount` IS the scaled amount we need, so we can return it directly.

## Benefits

1. **Correctness**: Using AToken revision ensures the formula matches the contract implementation
2. **Clarity**: Removed misleading comments about "Rev 8" being special
3. **Optimization**: Rev 9+ now uses the pre-scaled amount directly
4. **Maintainability**: Clear structure with numbered steps and explicit revision checks
5. **Future-proof**: Easier to understand and modify for future revisions

## Code Structure

The refactored function has a clear 4-step structure:

1. **BalanceTransfer handling** (all revisions)
   - Applies universally
   - Uses pre-scaled amount from event

2. **Pre-scaled amount optimization** (Rev 9+)
   - AToken Rev 5+ with Pool Rev 9+
   - Returns `minted_to_treasury_amount` directly

3. **Simple formula** (AToken Rev 1-3)
   - Uses half-up rounding
   - Straightforward calculation

4. **Complex formula** (AToken Rev 4+)
   - Reverses floor rounding
   - More involved calculation

## Verification

The refactored code maintains backward compatibility:
- AToken Rev 1-3 still uses simple formula
- AToken Rev 4+ still uses complex formula
- Rev 9+ now properly uses pre-scaled amount
- All existing tests should pass

## Related Issues

- Issue 0014: MINT_TO_TREASURY AccruedToTreasury Calculation Error
- Issue 0015: MINT_TO_TREASURY BalanceTransfer Amount Not Used
- Issue 0017: MINT_TO_TREASURY Validation Error on Pool Revision 8
- Issue 0019: MINT_TO_TREASURY Formula Error for Pool Revision 1
- Issue 0019: MINT_TO_TREASURY Rounding Mode Mismatch for Pool Revision 1

## References

- Plan: `plan/aave/Refactor MINT_TO_TREASURY Version Checks.md`
- Code: `src/degenbot/cli/aave.py` (lines 2648-2743)
- Pool Rev 1: `contract_reference/aave/Pool/rev_1.sol:3931-3956`
- Pool Rev 9: `contract_reference/aave/Pool/rev_9.sol:109-134`
- AToken Rev 1: `contract_reference/aave/AToken/rev_1.sol:2756-2778`
- AToken Rev 4: `contract_reference/aave/AToken/rev_4.sol`
