# Issue 0014: GHO Debt Repay Balance Delta Calculation Bug

**Date:** March 1, 2026

## Symptom

```
AssertionError: User 0x5b85B47670778b204041D6457dB8b5F5D36fa97a: debt balance (75403592044877533741860) does not match scaled token contract (75402927816733346332418) at block 18240233
```

Database balance exceeds contract balance by 664,228,144,187,409,442 wei (~0.66 GHO).

## Transaction Details

- **Transaction Hash:** 0xd08a1044fed4f8e998a2a97bed37362713803a64e1b56c4ef2e29a0057cf08f2
- **Block:** 18240233
- **Type:** repayWithPermit (GHO debt repayment)
- **User:** 0x5b85B47670778b204041D6457dB8b5F5D36fa97a
- **Asset:** GHO (0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f)
- **Debt Token:** variableDebtEthGHO (0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B, Revision 2)
- **Repayment Amount:** 24,044,260,719,194,335,056 GHO (~24.04 GHO)
- **User Discount:** 0.71% (71 basis points)

## Root Cause

The bug is in the GHO debt mint event processor (v1.py and v2.py) when handling Mint events emitted from `_burnScaled` during debt repayment when interest exceeds repayment.

### Contract Behavior

In `GhoVariableDebtToken._burnScaled()` (revision 2), when `balanceIncrease > amount` (accrued interest exceeds repayment):

1. `_accrueDebtOnAction()` calculates interest and discount
2. Contract burns: `_burn(user, (amount_scaled + discount_scaled))`
3. Contract emits Mint event with `value = balanceIncrease - amount` (net interest after repayment)

### The Bug

**Current code** (v1.py line 96, v2.py line 114):
```python
elif event_data.balance_increase > event_data.value:
    # GHO REPAY: emitted in _burnScaled
    amount_repaid = event_data.balance_increase - event_data.value
    repayment_scaled = wad_ray_math.ray_div(
        a=amount_repaid,
        b=event_data.index,
    )
    
    # ... full repayment check ...
    
    # Partial repayment: net change is interest with discount minus repayment
    balance_delta = discount_scaled - repayment_scaled  # <-- WRONG
```

**Problem:** The formula `discount_scaled - repayment_scaled` is incorrect. The contract actually burns `(amount_scaled + discount_scaled)` from the scaled balance, so the change should be `-(amount_scaled + discount_scaled)`.

### Correct Formula

The net scaled balance change should be:

```python
balance_delta = -(repayment_scaled + discount_scaled)
```

Where:
- `repayment_scaled = amount_repaid / index` (the repayment amount in scaled terms)
- `discount_scaled` = discount amount in scaled terms (from accrue_debt_on_action)

### Why This Causes the Bug

With the actual values from the failing transaction:
- `repayment_scaled` = 23,963,502,211,524,933,093
- `discount_scaled` = 332,114,072,093,704,721

Current (buggy) code: `balance_delta = 332,114,072,093,704,721 - 23,963,502,211,524,933,093 = -23,631,388,139,431,228,372`

Correct formula: `balance_delta = -(23,963,502,211,524,933,093 + 332,114,072,093,704,721) = -24,295,616,283,618,637,814`

Difference: 664,228,144,187,409,442 wei (matches the error!)

## Fix

**File:** `src/degenbot/aave/processors/debt/gho/v1.py` and `v2.py`

**Change:** In the `elif event_data.balance_increase > event_data.value:` branch (GHO repay case), replace:

```python
# Partial repayment: net change is interest with discount minus repayment
balance_delta = discount_scaled - repayment_scaled
```

With:

```python
# Partial repayment: burn (repayment_scaled + discount_scaled)
balance_delta = -(repayment_scaled + discount_scaled)
```

**Note:** The full repayment check (`if amount_repaid == balance_before_burn`) should still use `balance_delta = -previous_balance` as it correctly burns the entire balance.

## Key Insight

The GHO variable debt token uses a unique mechanism where:
1. Interest accrues by updating the user's `lastIndex` (not by changing scaled balance)
2. When repaying, the contract burns `(amount_scaled + discount_scaled)` from the scaled balance
3. The Mint event's `value` and `balanceIncrease` fields are informational and track interest, but the actual scaled balance change is just the burn amount

The formula must match the contract's actual burn operation: `_burn(user, (amount_scaled + discount_scaled))`

## Refactoring

1. **Add clarifying comments** in the GHO processors explaining the burn operation
2. **Consider adding debug logging** for complex GHO operations to aid future troubleshooting
3. **Add unit tests** for various discount rates and repayment scenarios

## Test Verification

After applying the fix:
```bash
uv run degenbot aave update --to-block=18240233
```

The update should complete successfully without the assertion error.
