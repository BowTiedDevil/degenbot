# Issue 0025: LIQUIDATION Net Debt Increase - Mint Event Misclassification

**Date:** March 17, 2026

## Symptom

Balance verification failure during Aave update:
```
AssertionError: Balance verification failure for AaveV3Asset(...USDC...).
User 0x4A76a94442FAFF09b67689b4Ba5645C47638F38a scaled balance (524827144398) 
does not match contract balance (262455954894) at block 20872104
```

## Root Cause

The Mint event at logIndex=6 during the first liquidation was incorrectly classified as an **INTEREST_ACCRUAL** operation instead of being processed as part of the **LIQUIDATION** operation. This event represented a **net debt increase** (not pure interest accrual) because accrued interest exceeded the repayment amount.

### Investigation Discovery

The transaction `0x75b41542ba21912e8210166c11a10d0bbb70514ffce26bf1b42b2f723abee5e2` at block 20872104 contains:

1. **Two liquidations** of the same user with the same debt asset (USDC):
   - Liquidation 1 (logIndex=17): debtToCover = 47,005,978 USDC, collateral = WETH
   - Liquidation 2 (logIndex=30): debtToCover = 292,538,129,344 USDC, collateral = WBTC

2. **One vUSDC Burn event** at logIndex=19 (amount = 292,538,129,344) - matches Liquidation 2

3. **One vUSDC Mint event** at logIndex=6 (amount = 352,195,531, balanceIncrease = 399,201,509) - from Liquidation 1

### Contract Behavior

In `VariableDebtToken._burnScaled()` (rev_1.sol:2662-2688):
```solidity
function _burnScaled(address user, address target, uint256 amount, uint256 index) internal {
    uint256 amountScaled = amount.rayDiv(index);
    uint256 scaledBalance = super.balanceOf(user);
    uint256 balanceIncrease = scaledBalance.rayMul(index) - 
        scaledBalance.rayMul(_userState[user].additionalData);
    
    _burn(user, amountScaled.toUint128());
    
    if (balanceIncrease > amount) {
        // Interest > repayment: NET DEBT INCREASE
        uint256 amountToMint = balanceIncrease - amount;  // 352,195,531
        emit Transfer(address(0), user, amountToMint);
        emit Mint(user, user, amountToMint, balanceIncrease, index);
    } else {
        // Repayment >= interest: net debt decrease
        uint256 amountToBurn = amount - balanceIncrease;
        emit Transfer(user, address(0), amountToBurn);
        emit Burn(user, target, amountToBurn, balanceIncrease, index);
    }
}
```

For Liquidation 1:
- debtToCover = 47,005,978 (amount parameter to burn)
- balanceIncrease = 399,201,509 (accrued interest)
- Since `balanceIncrease > amount`, the contract emitted a **Mint event** with value = 352,195,531 (net debt increase)

### Processing Error

The Mint event at logIndex=6 was:
1. **Created as an INTEREST_ACCRUAL operation** because `balance_increase >= amount` matches the interest accrual detection pattern
2. **Processed with scaled_amount = 0** because INTEREST_ACCRUAL operations are considered tracking-only
3. **Not matched to the first LIQUIDATION operation**, so the net debt increase was never applied

**Result:** The calculated balance (524,827,144,398) was double the expected balance (262,455,954,894) because:
- Starting balance: 524,911,475,260
- Missing net debt increase: +352,195,531 (from Mint event)
- Actual burn: -292,538,129,344 (from Burn event)
- Expected final: 262,455,954,894
- Calculated final: 524,827,144,398 (missing the net increase)

## Transaction Details

- **Hash:** `0x75b41542ba21912e8210166c11a10d0bbb70514ffce26bf1b42b2f723abee5e2`
- **Block:** 20872104
- **Type:** Flash Loan Liquidation (double liquidation of same user)
- **User:** `0x4A76a94442FAFF09b67689b4Ba5645C47638F38a` (liquidated)
- **Asset:** USDC (`0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48`)
- **vToken:** `0x72E95b8931767C79bA4EeE721354d6E99a61D004` (revision 1)
- **Pool:** `0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2` (revision 4)

## Smart Contract Control Flow

1. **Flash Loan:** Balancer Vault → BebopSettlement (292,585 USDC)
2. **Liquidation Call 1** (WETH collateral):
   - Pool.liquidationCall() with debtToCover=47,005,978
   - VariableDebtToken._burnScaled() called
   - Interest accrued: 399,201,509 > Repayment: 47,005,978
   - **Emits Mint event** (net debt increase of 352,195,531)
   - Collateral burned: 19,903,984 WETH
3. **Liquidation Call 2** (WBTC collateral):
   - Pool.liquidationCall() with debtToCover=292,538,129,344
   - VariableDebtToken._burnScaled() called
   - Interest accrued: 0 (already updated)
   - **Emits Burn event** (debt reduction of 292,538,129,344)
   - Collateral burned: 499 WBTC
4. **Flash Loan Repayment:** BebopSettlement → Balancer Vault

## Key Events

| Log Index | Event | Contract | Description |
|-----------|-------|----------|-------------|
| 5 | Transfer | vUSDC | Interest accrual notification (352,195,531 underlying) |
| 6 | Mint | vUSDC | **Net debt increase** from Liquidation 1 (NOT interest accrual) |
| 17 | LiquidationCall | Pool | First liquidation (47,005,978 USDC, WETH collateral) |
| 18 | Transfer | vUSDC | Debt burn notification from Liquidation 2 |
| 19 | Burn | vUSDC | Debt burn from Liquidation 2 (292,538,129,344) |
| 30 | LiquidationCall | Pool | Second liquidation (292,538,129,344 USDC, WBTC collateral) |

## Fix

### 1. Enrichment Layer (`src/degenbot/aave/enrichment.py`)

Added special handling for LIQUIDATION operations with DEBT_MINT events where `balance_increase > amount`:

```python
elif (
    operation.operation_type.name in {"LIQUIDATION", "GHO_LIQUIDATION"}
    and scaled_event.event_type == ScaledTokenEventType.DEBT_MINT
    and scaled_event.balance_increase is not None
    and scaled_event.balance_increase > scaled_event.amount
):
    # Interest exceeds repayment: net debt increase
    # Use DEBT_BURN calculation (floor rounding) to correctly handle the net increase
    calculation_event_type = ScaledTokenEventType.DEBT_BURN
    raw_amount = scaled_event.balance_increase - scaled_event.amount
    logger.debug(
        f"ENRICHMENT: LIQUIDATION net debt increase - using DEBT_BURN "
        f"calculation with raw_amount={raw_amount}"
    )
```

### 2. Operation Creation (`src/degenbot/cli/aave_transaction_operations.py`)

Added logic to find and assign Mint events that represent net debt increase during liquidation to the correct LIQUIDATION operation:

```python
# Find debt mint events that represent net debt increase during liquidation
debt_mint: ScaledTokenEvent | None = None
for ev in scaled_events:
    if ev.event["logIndex"] in assigned_indices:
        continue
    if ev.user_address != user:
        continue
    if is_gho and ev.event_type != ScaledTokenEventType.GHO_DEBT_MINT:
        continue
    if not is_gho and ev.event_type != ScaledTokenEventType.DEBT_MINT:
        continue

    event_token_address = get_checksum_address(ev.event["address"])
    if debt_v_token_address is not None and event_token_address == debt_v_token_address:
        # Check if this is a net debt increase (interest > repayment)
        if ev.balance_increase is not None and ev.balance_increase > ev.amount:
            debt_mint = ev
            break

# Add to scaled_token_events if found
if debt_mint is not None:
    scaled_token_events.append(debt_mint)
```

## Key Insight

**Critical distinction:** Not all Mint events represent the same operation type:

- **BORROW Mint:** `balance_increase == 0`, actual debt minting, increases scaled balance
- **INTEREST_ACCRUAL Mint:** `balance_increase == amount`, tracking-only event, does NOT change scaled balance
- **REPAY Mint:** `balance_increase > amount` in REPAY operation, net debt increase
- **LIQUIDATION Mint:** `balance_increase > amount` in LIQUIDATION operation, net debt increase when interest > repayment

The detection logic `balance_increase >= amount` matches BOTH interest accrual AND net debt increase cases. The **operation type context** (LIQUIDATION vs INTEREST_ACCRUAL) is required to correctly classify and process these events.

## Refactoring

1. **Enhanced operation creation:** Match Mint events to LIQUIDATION operations when `balance_increase > amount`
2. **Improved enrichment:** Use context-aware calculation (DEBT_BURN for liquidation net increases)
3. **Better documentation:** Clarify that `balance_increase >= amount` pattern has multiple semantic meanings
4. **Consider edge case detection:** Add validation to catch when Mint events during liquidations are misclassified

## Related Issues

- Issue 0004: Interest Accrual Scaling Error in Enrichment
- Issue 0007: Interest Accrual Burn Amount Zeroed in Enrichment
- Issue 0008: INTEREST_ACCRUAL Debt Burn Missing Pool Event Reference

## Contract References

- VariableDebtToken rev_1: `_burnScaled()` function at lines 2662-2688
- VariableDebtToken rev_1: `_transfer()` function at lines 2698-2728
- Pool rev_4: `_burnDebtTokens()` function at lines 9140-9167
- Pool rev_4: `executeLiquidationCall()` function at lines 8912-9059

## Files Modified

1. `src/degenbot/aave/enrichment.py` - Added LIQUIDATION net debt increase handling
2. `src/degenbot/cli/aave_transaction_operations.py` - Added debt mint event matching to liquidation operations

## Verification

✅ **Before fix:** Balance verification failed with calculated balance (524,827,144,398) ≠ expected (262,455,954,894)
✅ **After fix:** Successfully updated to block 20,872,107 without balance verification errors
