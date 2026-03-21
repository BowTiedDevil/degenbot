# Issue 0045: Liquidation Validation Treats Debt Mint as Multiple Burns

**Date:** March 21, 2026

## Symptom

Transaction validation failure during Aave update:
```
TRANSACTION VALIDATION FAILED

Transaction Hash: 0x75b41542ba21912e8210166c11a10d0bbb70514ffce26bf1b42b2f723abee5e2
Block: 20872104

VALIDATION ERRORS:
----------------------------------------
Operation 0 (LIQUIDATION) validation failed:
Multiple debt burns for same asset in LIQUIDATION. Debt burns: [19, 6]. Token addresses: ['0x72E95b8931767C79bA4EeE721354d6E99a61D004']
```

## Root Cause

### Part 1: Validation Using Too-Broad Event Filter

The `_validate_liquidation` method uses `is_debt` property to identify debt burns, but `is_debt` includes BOTH burns AND mints:

```python
@property
def is_debt(self) -> bool:
    return self.event_type in {
        ScaledTokenEventType.DEBT_BURN,
        ScaledTokenEventType.DEBT_MINT,  # <-- PROBLEM: Mint is not a burn!
        ScaledTokenEventType.DEBT_TRANSFER,
        ...
    }
```

In the validation logic at line 3197:
```python
debt_burns = [e for e in op.scaled_token_events if e.is_debt]
```

This collects both:
- **DEBT_BURN** at logIndex 19: Actual debt burn (292,538,129,344 vUSDC)
- **DEBT_MINT** at logIndex 6: Net debt increase from interest > repayment (352,195,531 vUSDC)

Since both events have the same token address (vUSDC at 0x72E95b8931767C79bA4EeE721354d6E99a61D004), the validation incorrectly reports "Multiple debt burns for same asset".

### Part 2: Debt Burn Matching to Wrong Liquidation

The transaction contains **two liquidations** of the same user with USDC debt:
- **Liquidation 1** (WETH collateral, logIndex=17): debtToCover = 47,005,978
  - Has a DEBT_MINT (net debt increase, no actual burn)
- **Liquidation 2** (WBTC collateral, logIndex=30): debtToCover = 292,538,129,344
  - Has a DEBT_BURN at logIndex=19 (amount = 292,538,129,344)

The `_collect_primary_debt_burns` method was matching the DEBT_BURN at logIndex=19 to Liquidation 1 instead of Liquidation 2, because it only checked user+asset matching, not the amount.

## Transaction Details

- **Hash:** `0x75b41542ba21912e8210166c11a10d0bbb70514ffce26bf1b42b2f723abee5e2`
- **Block:** 20872104
- **Type:** Double liquidation (flash loan)
- **User:** `0x4A76a94442FAFF09b67689b4Ba5645C47638F38a`
- **Debt asset:** USDC
- **vToken:** `0x72E95b8931767C79bA4EeE721354d6E99a61D004` (revision 1)
- **Pool:** `0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2` (revision 4)

## Smart Contract Behavior

From `VariableDebtToken._burnScaled()` (rev_1.sol):
```solidity
function _burnScaled(address user, address target, uint256 amount, uint256 index) internal {
    uint256 amountScaled = amount.rayDiv(index);
    uint256 scaledBalance = super.balanceOf(user);
    uint256 balanceIncrease = scaledBalance.rayMul(index) - 
        scaledBalance.rayMul(_userState[user].additionalData);
    
    _burn(user, amountScaled.toUint128());
    
    if (balanceIncrease > amount) {
        // NET DEBT INCREASE - emits Mint
        uint256 amountToMint = balanceIncrease - amount;
        emit Mint(user, user, amountToMint, balanceIncrease, index);
    } else {
        // NET DEBT DECREASE - emits Burn
        uint256 amountToBurn = amount - balanceIncrease;
        emit Burn(user, target, amountToBurn, balanceIncrease, index);
    }
}
```

For Liquidation 1:
- debtToCover (amount) = 47,005,978
- balanceIncrease = 399,201,509
- Since `balanceIncrease > amount`, contract emits **Mint event** with `amountToMint = 352,195,531`

## Fix

### Fix 1: Validation Filter (aave_transaction_operations.py:3197)

Changed from:
```python
debt_burns = [e for e in op.scaled_token_events if e.is_debt]
```

To:
```python
debt_burns = [e for e in op.scaled_token_events if e.is_burn and e.is_debt]
```

This ensures only actual burn events are counted when checking for "multiple debt burns for same asset".

### Fix 2: Debt Burn Matching with Amount Check (aave_transaction_operations.py:1951-1999)

Enhanced `_collect_primary_debt_burns` to use `debt_to_cover` parameter to distinguish between multiple liquidations:

```python
# When multiple liquidations share the same user and debt asset,
# use the debt_to_cover from the LiquidationCall to identify the
# correct burn event. The burn amount + balance_increase should
# approximately match the debtToCover from the pool event.
total_burn = ev.amount + (ev.balance_increase or 0)
if abs(total_burn - debt_to_cover) > TOKEN_AMOUNT_MATCH_TOLERANCE:
    # Burn doesn't match this liquidation's debtToCover,
    # likely belongs to a different liquidation
    continue
```

### Fix 3: Processing Layer Support for LIQUIDATION (aave.py:3330-3380)

Enhanced `_process_debt_mint_with_match` to handle LIQUIDATION operations with Mint events:

```python
if operation.operation_type in {
    OperationType.GHO_REPAY,
    OperationType.REPAY,
    OperationType.REPAY_WITH_ATOKENS,
    OperationType.LIQUIDATION,
    OperationType.GHO_LIQUIDATION,
}:
    # Decode the amount based on operation type
    if operation.operation_type in {
        OperationType.REPAY,
        OperationType.GHO_REPAY,
        OperationType.REPAY_WITH_ATOKENS,
    }:
        repay_amount, _ = eth_abi.abi.decode(
            types=["uint256", "bool"],
            data=operation.pool_event["data"],
        )
    else:  # LIQUIDATION or GHO_LIQUIDATION
        repay_amount, _, _, _ = eth_abi.abi.decode(
            types=["uint256", "uint256", "address", "bool"],
            data=operation.pool_event["data"],
        )
    # ... calculate scaled burn and process as DebtBurnEvent
```

## Files Modified

1. `src/degenbot/cli/aave_transaction_operations.py` (lines 3197, 1951-1999)
2. `src/degenbot/cli/aave.py` (lines 3330-3380)

## Key Insight

**The `is_debt` property is too broad for validation purposes.** It includes all debt-related events (burns, mints, transfers, interest events), but validation specifically needs to check for multiple burns of the same asset.

A liquidation can legitimately have:
- 1 debt burn (standard case)
- 0 debt burns (flash loan liquidation)
- 1 debt mint (net debt increase when interest > repayment)
- Multiple debt burns for different assets (multi-asset liquidation)

But it should NOT have multiple debt burns for the same asset - that's what the validation is checking for.

**When multiple liquidations share the same user and debt asset**, amount-based matching is required to correctly pair debt burns with their corresponding LiquidationCall events.

## Related Issues

- **Issue 0025**: LIQUIDATION Net Debt Increase - Mint Event Misclassification
  - Introduced the initial handling for DEBT_MINT in liquidations
  - The current fix addresses the validation and matching issues discovered later

- **Issue 0028**: Multi-Asset Debt Liquidation Missing Secondary Debt Burns
  - Introduced the semantic matching approach for debt burns
  - Enhanced here with amount-based disambiguation for same-asset liquidations

## Refactoring

1. **Fixed validation filter** - Use `is_burn and is_debt` instead of just `is_debt`
2. **Enhanced burn matching** - Added amount-based disambiguation for same-asset liquidations
3. **Added liquidation handling** - Extended processing layer to decode LIQUIDATION events correctly
4. **Consider adding `is_debt_burn` property** for clarity:
   ```python
   @property
   def is_debt_burn(self) -> bool:
       return self.event_type in {
           ScaledTokenEventType.DEBT_BURN,
           ScaledTokenEventType.DEBT_INTEREST_BURN,
           ScaledTokenEventType.GHO_DEBT_BURN,
           ScaledTokenEventType.GHO_DEBT_INTEREST_BURN,
       }
   ```

## Verification

After fix:
- Transaction `0x75b415...` processes successfully
- Liquidation 1 contains: DEBT_MINT (net increase), COLLATERAL_BURN, transfers
- Liquidation 2 contains: DEBT_BURN (292,538,129,344), COLLATERAL_BURN, transfers
- Validation passes - no "multiple debt burns" error
- Balance verification passes

## Lint & Type Check

- ✅ `uv run ruff check` - No issues
- ✅ `uv run mypy` - No issues
