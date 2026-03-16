# Issue 0007: Interest Accrual Burn Amount Zeroed in Enrichment

**Issue:** Interest Accrual Burn Amount Zeroed in Enrichment  
**Date:** 2026-03-15  
**Symptom:** 
```
AssertionError: Balance verification failure for AaveV3Asset(...). User AaveV3User(...) scaled balance (132821778521310521027326) does not match contract balance (0) at block 23088588
```

## Root Cause

Incorrect arithmetic in the burn event matching logic for WITHDRAW operations. The code subtracted `balanceIncrease` from `amount` when it should have added them.

**The Burn Event Structure:**
```solidity
event Burn(address indexed from, address indexed target, uint256 value, uint256 balanceIncrease, uint256 index);
```

- `value`: Principal amount being burned (in underlying units)
- `balanceIncrease`: Accrued interest since last user interaction
- **Total withdrawn** = `value` + `balanceIncrease`

**The Bug:**
```python
# WRONG - Line 1163 in aave_transaction_operations.py
if ev.amount - ev.balance_increase not in expected_burn_amounts:
    
# CORRECT
if ev.amount + ev.balance_increase not in expected_burn_amounts:
```

This single-character error (`-` instead of `+`) caused the burn event to fail matching with the WITHDRAW operation, leaving it as an unassigned event. Unassigned burns are classified as `INTEREST_ACCRUAL` operations, which are enriched with `scaled_amount = 0`. This meant the burn didn't reduce the user's balance.

## Transaction Details

- **Hash:** `0x6fb01da57e206605f477c433b4b7841c1d1f52cfa7305b8dd3219394ec9e3796`
- **Block:** 23088588
- **Type:** WITHDRAW (full withdrawal using `type(uint256).max`)
- **User:** `0x3A272221e648903ae5A4F5F5e3e36E97a68be1e2`
- **Asset:** CRV (`0xD533a949740bb3306d119CC777fa900bA034cd52`)
- **Pool Revision:** 9
- **aToken Revision:** 4
- **vToken Revision:** 4

### Event Sequence

1. **logIndex 146:** `ReserveDataUpdated` - Interest rate/liquidity index update
2. **logIndex 147:** `Transfer` (aToken) - 139,927.18 aCRV burned from user
3. **logIndex 148:** `Burn` (aToken) - Interest accrual: 26.17 CRV, principal: 139,927.18 CRV
4. **logIndex 149:** `Transfer` (CRV) - 139,927.20 CRV transferred to user
5. **logIndex 150:** `Withdraw` - Withdrawal confirmation

### The Mismatch

| Field | Value |
|-------|-------|
| Burn `amount` | 139,927.175890041242521333 |
| Burn `balanceIncrease` | 0.000026172977322687352 |
| **Burn Total** (amount + balanceIncrease) | **139,927.202063018565208685** |
| Withdraw `amount` | **139,927.202063018565208685** |

**With subtraction:** 139,927.175890041242521333 - 0.000026172977322687352 = 139,927.149717063919834381 ❌

**With addition:** 139,927.175890041242521333 + 0.000026172977322687352 = 139,927.202063018565208685 ✅

## Fix

**Location:** `src/degenbot/cli/aave_transaction_operations.py`  
**Line:** 1163

**Change:**
```python
# Before:
if ev.amount - ev.balance_increase not in expected_burn_amounts:

# After:
if ev.amount + ev.balance_increase not in expected_burn_amounts:
```

**Result:**
- Burn event now correctly matches WITHDRAW operation
- Scaled amount is calculated normally (not zeroed)
- User balance is correctly reduced to 0
- Verification passes

## Key Insight

**The Burn event's `amount` field contains the principal only, not the total.** The total burned (and withdrawn) is `amount + balanceIncrease`. This matches the Withdraw event's amount exactly.

This bug only manifested because:
1. Pool revision 9 introduced pre-scaling with 2-wei tolerance
2. The subtraction produced a value far outside the tolerance
3. The burn became unassigned and was treated as interest accrual
4. Interest accrual operations have `scaled_amount = 0`

## Refactoring

**Proposed improvements:**

1. **Add comprehensive unit tests** for WITHDRAW operations with interest accrual across all Pool revisions

2. **Document the Burn event structure** clearly in code comments to prevent similar confusion:
   ```python
   # Burn event amount = principal only
   # Total burned = amount + balanceIncrease
   # This matches the Withdraw event amount
   ```

3. **Consider removing the tolerance check** for revision 9+ if the math is correct, or tighten the tolerance if the math is verified

4. **Add assertion** to catch unmatched burns in INTEREST_ACCRUAL operations:
   ```python
   if operation.operation_type == OperationType.INTEREST_ACCRUAL:
       assert scaled_event.event_type in MINT_EVENT_TYPES, \
           f"Burn event should not be classified as INTEREST_ACCRUAL"
   ```

**Filename:** 0007 - Interest Accrual Burn Amount Zeroed in Enrichment
