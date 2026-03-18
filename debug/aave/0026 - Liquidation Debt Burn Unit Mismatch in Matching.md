# Issue 0026: Liquidation Debt Burn Matching Uses Wrong Amount

**Issue:** Liquidation Debt Burn Matching Uses Wrong Amount  
**Date:** 2026-03-18  
**Symptom:** 
```
AssertionError: Balance verification failure for AaveV3Asset(...). User AaveV3User(...) scaled balance (35626) does not match contract balance (0) at block 21928936
```

## Root Cause

In `_create_liquidation_operation`, the code incorrectly calculated the burn amount by adding `balance_increase` to the Burn event's `amount` field. The Burn event's `value` field already represents the **net** amount burned (after accounting for interest), not the gross amount.

**The Burn Event Structure (from VariableDebtToken rev_1.sol):**
```solidity
event Burn(
    address indexed from,
    address indexed target,
    uint256 value,           // amountToBurn = amount - balanceIncrease
    uint256 balanceIncrease, // Interest accrued since last interaction
    uint256 index
);
```

**From the contract (`_burnScaled` function, lines 2683-2686):**
```solidity
} else {
    uint256 amountToBurn = amount - balanceIncrease;
    emit Transfer(user, address(0), amountToBurn);
    emit Burn(user, target, amountToBurn, balanceIncrease, index);
}
```

- `amount`: The underlying amount passed to `burn()` (e.g., debtToCover from liquidation)
- `balanceIncrease`: Interest accrued since last user interaction
- **`value` in Burn event**: `amount - balanceIncrease` (the NET amount actually burned)

**The Bug (lines 1887-1893 in aave_transaction_operations.py):**
```python
burn_amount = ev.amount  # This is ALREADY amountToBurn (net)
if ev.balance_increase is not None and ev.balance_increase > 0:
    # BUG: Adding balance_increase when it's already accounted for!
    burn_amount = ev.amount + ev.balance_increase  # Double-counting interest
```

**Why It Fails:**
- Burn event: value=35,780 (already net), balanceIncrease=619
- debtToCover from LiquidationCall: 35,239
- Calculated `burn_amount`: 35,780 + 619 = **36,399** (wrong - double counted)
- Difference: |35,239 - 36,399| = **1,160**
- Tolerance: max(35,239 × 0.01, 1,000) = **1,000**
- Since 1,160 > 1,000, the burn event is **rejected** as a match

The burn becomes unassigned and is classified as `INTEREST_ACCRUAL`, which then tries to burn the debt again, causing the balance verification failure.

## Transaction Details

- **Hash:** `0x09f27f2ee2a04a13a85e137007135593d848ffd5d590980783cfcb2d2571ab04`
- **Block:** 21928936
- **Type:** LIQUIDATION (dual liquidation transaction)
- **User:** `0x9913e51274235E071967BEb71A2236A13F597A78`
- **Debt Asset:** WBTC (`0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599`)
- **Collateral Asset:** WETH (`0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2`)
- **Pool Revision:** 1
- **vToken Revision:** 1

### Event Sequence (User 0x9913...7a78)

| Log Index | Event | Key Data |
|-----------|-------|----------|
| 0x177 | Transfer (vToken) | Burn 35,780 from user |
| 0x178 | Burn (vToken) | Amount: 35,780, BalanceIncrease: 619, Index: 1.02170766... |
| 0x17a | ReserveDataUpdated | WBTC reserve data update |
| 0x17b | ReserveDataUpdated | WETH reserve data update |
| 0x184 | LiquidationCall | debtToCover: 35,239 |

### The Mismatch

| Field | Value | Description |
|-------|-------|-------------|
| Burn `amount` (ev.amount) | 35,780 | **NET** amount burned (already subtracted balanceIncrease) |
| Burn `balanceIncrease` | 619 | Interest accrued |
| Liquidation `debtToCover` | 35,239 | Amount the liquidator pays |

**Current buggy calculation:**
```python
burn_amount = 35,780 + 619 = 36,399  # WRONG - double counting
```

**Correct calculation should be:**
```python
burn_amount = 35,780  # CORRECT - use as-is, it's already the net amount
```

## Fix

**Location:** `src/degenbot/cli/aave_transaction_operations.py`  
**Method:** `_create_liquidation_operation`  
**Lines:** 1885-1893

**Change:**
```python
# Before (BUGGY):
# For multiple liquidations with the same debt asset, match based on amount
# The burn event's amount should closely match the liquidation's debtToCover
# (allowing for small differences due to interest accrual)
burn_amount = ev.amount
if ev.balance_increase is not None and ev.balance_increase > 0:
    # Adjust burn amount by balance increase if present
    burn_amount = ev.amount + ev.balance_increase

# After (CORRECT):
# For multiple liquidations with the same debt asset, match based on amount
# The burn event's amount should closely match the liquidation's debtToCover
# (allowing for small differences due to interest accrual)
# Note: The Burn event's `value` field (ev.amount) is the amountToBurn from the
# contract, which is already the NET amount (amount - balanceIncrease).
# Do NOT add balance_increase - it's already accounted for in the net burn amount.
burn_amount = ev.amount
```

**Result:**
- Burn event correctly matches LIQUIDATION operation
- Debt burn is processed once (during liquidation)
- No duplicate INTEREST_ACCRUAL operation is created
- User balance is correctly reduced to 0
- Verification passes

## Key Insight

**The Burn event's `value` field is NOT the gross amount - it's the net amount after interest.**

Looking at the Aave contract source (`VariableDebtToken._burnScaled`):
1. `amount` parameter = underlying amount to burn (e.g., debtToCover)
2. `balanceIncrease` = interest accrued since last interaction
3. If `balanceIncrease > amount`: Mint the difference (net interest gain)
4. If `balanceIncrease <= amount`: Burn the difference (amount - balanceIncrease)

The Burn event is only emitted in case #4, and `value = amount - balanceIncrease`.

This bug only manifested because:
1. The debtToCover (35,239) and the incorrectly calculated burn_amount (36,399) differed by more than the 1,000 tolerance
2. The burn was rejected as a match and became unassigned
3. Unassigned burns are classified as INTEREST_ACCRUAL
4. The INTEREST_ACCRUAL operation tried to burn the debt again

## Refactoring

**Proposed improvements:**

1. **Add clear documentation** about Burn event semantics:
   ```python
   # Burn event amount = NET amount burned (amount - balanceIncrease)
   # This is emitted only when the user's debt is being reduced
   # If balanceIncrease > amount, a Mint event is emitted instead
   ```

2. **Add unit tests** for liquidation matching with various balance_increase values to prevent regression.

3. **Add assertion** in enrichment to validate burn event semantics:
   ```python
   # In enrichment, verify that amount >= balance_increase for Burn events
   assert scaled_event.amount >= scaled_event.balance_increase, \
       f"Burn event amount ({scaled_event.amount}) should be >= balance_increase ({scaled_event.balance_increase})"
   ```

4. **Review other burn event usages** to ensure this pattern isn't repeated elsewhere:
   - `_match_burn_to_repay`: Uses `ev.amount + ev.balance_increase` - NEEDS REVIEW
   - `_match_burn_to_withdraw`: Uses `ev.amount + ev.balance_increase` - NEEDS REVIEW

**Filename:** 0026 - Liquidation Debt Burn Matching Uses Wrong Amount
