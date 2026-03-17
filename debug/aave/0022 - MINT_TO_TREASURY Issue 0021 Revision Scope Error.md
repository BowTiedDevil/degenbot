# MINT_TO_TREASURY Issue 0021 Revision Scope Error

## Issue

Issue 0021 introduced a simplified direct query approach that queries `accruedToTreasury` from the Pool contract. This approach was incorrectly assumed to work for **all** Pool revisions, but it only works correctly for **Pool Revision 9+**.

## Date

2026-03-17

## Symptom

```
AssertionError: Balance verification failure for AaveV3Asset(...). 
User AaveV3User(...) scaled balance (726897493651248500109) 
does not match contract balance (726912644679967779687) at block 20330845

Difference: 15,151,028,719,279,578 wei (~0.015 ETH)
```

## Root Cause

### Issue 0021's Incorrect Assumption

Issue 0021 stated:
> "The `accruedToTreasury` value in storage IS the scaled amount that gets minted."

**This is only true for Pool Revision 9+.**

### Contract Behavior Difference

**Pool Revision 9+:**
```solidity
uint256 amountToMint = accruedToTreasury.getATokenBalance(normalizedIncome);
IAToken(reserve.aTokenAddress).mintToTreasury(accruedToTreasury, normalizedIncome);
// AToken receives accruedToTreasury directly (scaled)
```

**Pool Revision 1-8:**
```solidity
uint256 amountToMint = accruedToTreasury.rayMul(normalizedIncome);
IAToken(reserve.aTokenAddress).mintToTreasury(amountToMint, normalizedIncome);
// AToken receives amountToMint (unscaled), then does rayDiv to get scaled
```

### Why the Direct Query Fails for Rev 1-8

For Rev 1-8, the flow is:
1. Pool reads `accruedToTreasury` from storage (SCALED)
2. Pool does: `amountToMint = rayMul(accruedToTreasury, index)` (UNSCALED)
3. Pool calls `AToken.mintToTreasury(amountToMint, index)`
4. AToken does: `scaledAmount = rayDiv(amountToMint, index)` (SCALED)

**Mathematical issue:** `rayDiv(rayMul(x, y), y) ≠ x` due to rounding at each step.

### Numbers for Failing Transaction

**Block:** 20330845  
**Transaction:** 0x314625d06bd48e8c617a2c33809e4f8cc38bb5741de7624874c209e47a2be357  
**Pool Revision:** 3  
**Asset:** WETH

| Value | Amount |
|-------|--------|
| `accruedToTreasury` (queried from Pool) | 57,812,248,948,972,503,613 |
| `amountToMint` (Pool's rayMul result) | 59,436,814,070,577,068,825 |
| Actual scaled amount minted | 57,827,399,977,691,783,191 |
| Difference | 15,151,028,719,279,578 wei |

## Solution

Use the **MintedToTreasury event's `amountMinted`** field with revision-specific handling:

### Implementation

```python
def _calculate_mint_to_treasury_scaled_amount(
    scaled_event: ScaledTokenEvent,
    balance_transfer_events: list[LogReceipt],
    tx_context: TransactionContext,
    operation: Operation,
) -> int:
    """
    Calculate scaled amount for MINT_TO_TREASURY operations.
    
    Uses the MintedToTreasury event amount which is available for all pool revisions.
    For Rev 1-8: amountMinted is in underlying units, needs rayDiv to get scaled amount.
    For Rev 9+: amountMinted equals the scaled amount directly (passed as-is to AToken).
    """
    # ... BalanceTransfer handling ...
    
    # Get the MintedToTreasury amount from the operation
    minted_amount = operation.minted_to_treasury_amount
    
    # For Pool Rev 9+, the minted amount IS the scaled amount
    if tx_context.pool_revision >= 9:
        return minted_amount
    
    # For Pool Rev 1-8: Convert underlying amount to scaled amount
    # Pool does: amountToMint = rayMul(accruedToTreasury, index) [underlying]
    # AToken does: scaledAmount = rayDiv(amountToMint, index) [scaled]
    scaled_amount = ray_div(minted_amount, scaled_event.index)
    return scaled_amount
```

### Why This Works

**Pool Rev 1-8:**
- `amountMinted` in event = `amountToMint` (unscaled)
- Calculate: `rayDiv(amountMinted, index)` → correct scaled amount

**Pool Rev 9+:**
- `amountMinted` in event = `accruedToTreasury` (scaled)
- Use directly → correct scaled amount

## Verification

### Before Fix
```
Using queried accruedToTreasury: 57,812,248,948,972,503,613
Calculated balance: 726,897,493,651,248,500,109 ❌
Contract balance: 726,912,644,679,967,779,687
Difference: 15,151,028,719,279,578 wei
```

### After Fix
```
Using MintedToTreasury amount: 59,436,814,070,577,068,825
RayDiv calculation: 57,827,399,977,691,783,191
Calculated balance: 726,912,644,679,967,779,687 ✓
Contract balance: 726,912,644,679,967,779,687
Difference: 0 wei
```

## Key Insight

**The MintedToTreasury event is the key to handling all revisions correctly.**

For Rev 1-8:
- Event emits `amountToMint` (unscaled)
- Must apply `rayDiv(amountToMint, index)` to get scaled amount

For Rev 9+:
- Event emits `accruedToTreasury` (scaled)
- Use directly as scaled amount

## Changes Made

### Files Modified

1. **`src/degenbot/cli/aave.py`**
   - Modified `_calculate_mint_to_treasury_scaled_amount()` to use MintedToTreasury event
   - Removed `_get_accrued_to_treasury_from_pool()` function
   - Added revision-specific handling (Rev 1-8 vs Rev 9+)

2. **`src/degenbot/cli/aave_transaction_operations.py`**
   - Updated `_create_mint_to_treasury_operations()` to extract MintedToTreasury for **all** revisions (not just Rev 8)

## References

- Issue 0014: Original complex formula implementation
- Issue 0019: MINT_TO_TREASURY Formula Error for Pool Revision 1
- Issue 0021: MINT_TO_TREASURY Simplified Direct Query Approach (scope error)
- Transaction: `0x314625d06bd48e8c617a2c33809e4f8cc38bb5741de7624874c209e47a2be357` (Block 20330845)
