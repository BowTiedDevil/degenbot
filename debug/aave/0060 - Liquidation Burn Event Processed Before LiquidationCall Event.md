# Issue 0060: Liquidation Burn Event Processed Before LiquidationCall Event

**Date:** 2026-03-25

## Issue ID
0060

## Symptom

Balance verification failure at block 21762841:
```
AssertionError: Debt balance verification failure for AaveV3Asset(..., symbol='USDT').
User AaveV3User(..., address='0xC002946773b39D6eC5d4345Ec1b25Bf5BdD07692')
scaled balance (111166212580) does not match contract balance (111038316113) at block 21762841
```

The calculated balance is **127,896,467 higher** than the actual contract balance.

## Transaction Details

- **Transaction Hash:** 0x90269a1852fdfd213680c1eb629f7563ed46a111115494f135bd7a267d6d9726
- **Block:** 21762841
- **Market:** Aave Ethereum Market (Pool revision 6)
- **User:** 0xC002946773b39D6eC5d4345Ec1b25Bf5BdD07692
- **Asset:** USDT (variableDebtEthUSDT at 0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8)
- **Operation:** Single liquidation (USDT collateral, USDT debt)

### Event Sequence (Critical Finding)

The events in this transaction appear in this order:

| LogIndex | Contract | Event Type | Key Data |
|----------|----------|------------|----------|
| 40 | Aave Pool | ReserveDataUpdated | USDT reserve index update |
| 41 | vToken | ERC20 Transfer | Mint from 0x0 to user |
| **42** | **vToken** | **Aave Burn** | **value=423201585, balanceIncrease=570565526** |
| 45 | aToken | ERC20 Transfer | Collateral transfer |
| 46 | aToken | Aave Mint | value=152746829, balanceIncrease=585351 |
| 50 | aToken | ERC20 Transfer | Collateral transfer |
| 51 | aToken | BalanceTransfer | Fee to treasury |
| **53** | **Aave Pool** | **LiquidationCall** | **debtToCover=147363941** |

**Critical Discovery:** The **Burn event (logIndex 42) is emitted BEFORE the LiquidationCall event (logIndex 53)**.

## Root Cause Analysis

### Initial Investigation

The initial hypothesis was that the burn event at logIndex 42 was being emitted before the LiquidationCall event at logIndex 53, and therefore couldn't be matched to the liquidation operation during parsing.

However, deeper investigation revealed the **actual root cause**:

### The Real Issue: Net Debt Increase During Liquidation

In Aave V3 VariableDebtToken, the `_burnScaled` function has this logic:

```solidity
if (balanceIncrease > amount) {
  // Net debt increase - emit Mint
  uint256 amountToMint = balanceIncrease - amount;
  emit Mint(user, user, amountToMint, balanceIncrease, index);
} else {
  // Net debt decrease - emit Burn
  uint256 amountToBurn = amount - balanceIncrease;
  emit Burn(user, target, amountToBurn, balanceIncrease, index);
}
```

In this liquidation:
- `amount` (debtToCover): **147,363,941**
- `balanceIncrease` (accrued interest): **570,565,526**

Since `balanceIncrease > amount`, the net effect is a **debt increase** of 423,201,585. Therefore, the contract correctly emits a **Mint** event, not a Burn event!

### The Bug in Processing

The code in `_process_debt_mint_with_match` (lines 3580-3590) had this logic:

```python
if operation.operation_type in LIQUIDATION_OPERATION_TYPES:
    liquidation_key = (user.address, token_address)
    aggregated_amount = tx_context.liquidation_aggregates.get(liquidation_key, 0)
    if aggregated_amount > 0:
        # Skip Mint event - will be handled by aggregation
        return
```

This logic was intended for multi-liquidation scenarios where debt burns are aggregated. However, it was incorrectly skipping the Mint event even for single liquidations, as long as any aggregation data existed.

### Mathematical Verification

From the LiquidationCall event at logIndex 53:
- `debtToCover`: **147,363,941** (actual debt reduced)
- Borrow index from Mint event: 1,152,212,759,579,850,982,488,909,387
- Expected scaled burn: **127,896,467** (using ray_div with half-up rounding)

The error difference of **127,896,467** exactly matches the expected scaled burn amount that was never applied.

## The Fix

The fix modifies the logic in `_process_debt_mint_with_match` to only skip Mint events when there are **multiple** liquidations that will be aggregated. For single liquidations, the Mint event (which represents net debt increase when interest > repayment) must be processed.

### Code Changes

**File:** `src/degenbot/cli/aave.py`

**Function:** `_process_debt_mint_with_match`

**Lines:** 3578-3590

**Before:**
```python
if operation.operation_type in LIQUIDATION_OPERATION_TYPES:
    liquidation_key = (user.address, token_address)
    aggregated_amount = tx_context.liquidation_aggregates.get(liquidation_key, 0)
    if aggregated_amount > 0:
        # Skip Mint event
        return
```

**After:**
```python
if operation.operation_type in LIQUIDATION_OPERATION_TYPES:
    liquidation_key = (user.address, token_address)
    liquidation_count = tx_context.liquidation_counts.get(liquidation_key, 0)
    # Only skip if there are multiple liquidations that will be aggregated
    if liquidation_count > 1:
        aggregated_amount = tx_context.liquidation_aggregates.get(liquidation_key, 0)
        if aggregated_amount > 0:
            logger.debug(
                f"_process_debt_mint_with_match: LIQUIDATION has {liquidation_count} "
                f"liquidations with aggregated amount {aggregated_amount}..."
            )
            return
```

### Why This Works

1. **Single liquidation**: The Mint event is processed, correctly reducing the debt position by the scaled amount calculated from `debtToCover`

2. **Multiple liquidations**: The Mint events are still skipped, allowing the aggregated burn processing to handle the total debt reduction from all liquidations

3. **Preserves existing behavior**: Multi-liquidation aggregation logic (debug/aave/0056) continues to work correctly

## Key Insight

**The real issue was NOT event ordering - it was incorrect handling of Mint events in liquidations.**

When investigating Aave V3 failures:
1. **Don't assume event types** - Check the contract code to understand when Mint vs Burn events are emitted
2. **Account for net debt changes** - When `balanceIncrease > debtToCover`, the net effect is a debt increase (Mint), not a debt decrease (Burn)
3. **Review skip conditions carefully** - Logic intended for multi-operation scenarios may incorrectly apply to single operations
4. **Check aggregation logic** - Aggregation shortcuts may skip events that should be processed

**The Aave V3 contract behavior:**
```solidity
// When interest > repayment: Mint event (net debt increase)
// When interest < repayment: Burn event (net debt decrease)
```

This is correct contract behavior - the code just needed to handle both cases properly.

## Related Issues

- Issue 0058: Single Liquidation Debt Burn Uses Unscaled Amount Error
- Issue 0059: Multi-Liquidation Debt Burn Uses Unscaled Amount Error

Both previous fixes addressed the calculation of scaled burn amounts but assumed the burn could be matched to a liquidation operation. This issue reveals the underlying architectural problem that prevents proper matching.

## References

- Transaction: 0x90269a1852fdfd213680c1eb629f7563ed46a111115494f135bd7a267d6d9726
- Block: 21762841
- User: 0xC002946773b39D6eC5d4345Ec1b25Bf5BdD07692
- Debt Asset: USDT (0xdAC17F958D2ee523a2206206994597C13D831ec7)
- Debt Token: variableDebtEthUSDT (0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8)
- Collateral Asset: USDT (aToken)
- Liquidator: 0x00000000009E50a7dDb7a7B0e2ee6604fd120E49
- Contract: VariableDebtToken revision 1
- Pool: Pool revision 6

## Summary

The failure occurred because the code incorrectly skipped processing Mint events in liquidation operations when any aggregation data existed. In this case, the liquidation had a Mint event (not a Burn event) because the accrued interest (570,565,526) exceeded the debt being repaid (147,363,941), resulting in a net debt increase.

The fix checks the liquidation count before skipping - only skipping Mint event processing when there are **multiple** liquidations that need aggregation.

**Changes Made:**
- Modified `_process_debt_mint_with_match` in `src/degenbot/cli/aave.py` (lines 3578-3590)
- Added check for `liquidation_count > 1` before skipping Mint events
- Updated debug logging to indicate when Mint events are skipped

**Status:** ✅ RESOLVED

**Verification:** 
- Block 21762841 now processes correctly
- User 0xC002...7692 debt balance matches contract: 111,038,316,113
- No regressions in other liquidation processing
