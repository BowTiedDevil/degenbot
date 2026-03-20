# 0039 - Cross-Asset Mint Matched to Wrong Withdraw Operation

**Issue:** USDS Mint event incorrectly matched to USDT Withdraw operation due to missing asset validation

**Date:** 2026-03-20

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...).
User 0xF20b338752976878754518183873602902360704 scaled balance (1944524635608115376707705) 
does not match contract balance (1944524635608105651761860) at block 23239173

Difference: 9,724,945,845 wei
```

## Investigation Summary

After extensive investigation including on-chain balance verification and transaction analysis, the root cause was identified as **cross-asset event matching** in the `_create_withdraw_operation()` function.

## Root Cause

In `_create_withdraw_operation()` (aave_transaction_operations.py:1220-1380), the code matches Mint events to Withdraw operations without verifying that the Mint event is for the **same asset** as the Withdraw operation.

### Transaction Analysis

**Transaction:** `0xbc0501df0c53d9c449f99bc4ea25f6a809f2bf69a3a530999b9eeab37c079e4f`

**Events for user 0xF20b...0704:**

**USDS (aUSDS at 0x32a6268f9Ba3642Dda7892aDd74f1D34469A4259):**
- logIndex 444: `Mint(amount=418053545862170317595, balance_increase=418053545862170317595, index=1028283361068134561082063716)`
- logIndex 458: `Burn(amount=1000000000000000000000, index=1028283361068134561082063716)` ← Withdrawal burn
- logIndex 460: `Withdraw(pool event for USDS)`

**USDT (aUSDT at 0x23878914EFE38d27C4D67Ab83ed1b93A74D4086a):**
- logIndex 463: `Mint(amount=679153324, balance_increase=10679153325, index=1135990659356204243742794198)`
- logIndex 465: `Withdraw(pool event for USDT)` ← Different asset!
- **No Burn event** (interest exceeds withdrawal)

### The Bug

The current code creates these operations:

```
Operation 1: WITHDRAW
  Pool event: logIndex=465 (USDT Withdraw!)
  Scaled event: logIndex=444 (USDS Mint) ← WRONG ASSET!
  Transfer event: logIndex=441
```

The USDS Mint at logIndex 444 is being matched to the USDT Withdraw at logIndex 465 because:
1. No Burn exists for USDT (correct - interest exceeds withdrawal)
2. Pattern 2 matching: `amount == balance_increase` for logIndex 444
3. **Missing check:** The code doesn't verify `mint_token_address == withdraw_reserve_address`

### Expected vs Actual Operations

**Current (Buggy):**
- Operation 1: WITHDRAW (USDT pool event + USDS Mint) ← MISMATCH!
- USDS Mint at 444: Added to WITHDRAW (incorrect)
- USDS Burn at 458: **Not assigned** (causes verification failure)

**Expected (Correct):**
- Operation 1: WITHDRAW (USDT pool event + USDT Mint at 463) ← Interest exceeds withdrawal
- USDS Mint at 444: **INTEREST_ACCRUAL** operation (scaled_amount = 0)
- USDS Burn at 458: WITHDRAW operation (primary burn)

### On-Chain Verification

**Balance Changes for 0xF20b...0704 on aUSDS:**
```
Before: 1,945,395,723,867,317,820,521,430
After:  1,944,524,635,608,105,651,761,860
Delta:    -871,088,259,212,168,759,570

Expected delta:
- USDS Interest accrual (logIndex 444): +418,053,545,862,170,317,595
- USDS Withdrawal burn (logIndex 458): -972,494,584,528,961,864,947
- Net: -554,441,038,666,791,547,352

Actual delta vs Expected: -316,647,220,545,377,212,218
```

The difference (-316,647,220,545,377,212,218) represents the USDS Mint that was incorrectly processed as a WITHDRAW instead of INTEREST_ACCRUAL.

## Why Event Ordering Isn't Sufficient

Event ordering alone cannot detect this issue because:
1. The USDS Mint (logIndex 444) comes **before** the USDT Withdraw (logIndex 465)
2. The code processes Withdraw pool events sequentially
3. When processing the USDT Withdraw, it searches for matching Mint events
4. It finds the USDS Mint (logIndex 444) because Pattern 2 matches (`amount == balance_increase`)
5. **No asset validation** prevents this cross-asset matching

## Fix Required

### Primary Fix: Add Asset Validation

In `_create_withdraw_operation()`, verify that the Mint event is for the same asset as the Withdraw:

```python
def _create_withdraw_operation(self, ...):
    # ... existing code ...
    
    # Get the reserve (asset) from the Withdraw event
    withdraw_reserve = decode_address(withdraw_event["topics"][1])
    
    # When searching for Mint/Burn events, filter by asset
    for ev in scaled_events:
        # ... existing checks ...
        
        # NEW: Verify asset matches
        event_token_address = get_checksum_address(ev.event["address"])
        reserve_asset = self._get_reserve_by_token(event_token_address)
        
        if reserve_asset != withdraw_reserve:
            # Skip events from different assets
            continue
        
        # ... rest of matching logic ...
```

### Secondary Fix: Pattern 2 Enhancement

Pattern 2 (`amount == balance_increase`) is too broad. Add additional validation:

```python
# Pattern 2: mint amount ≈ balance_increase (full interest used)
# Only match if:
# 1. amount == balance_increase
# 2. This is the ONLY Mint event for this user/asset
# 3. No Burn exists for this user/asset

has_burn_for_asset = any(
    ev.event_type == ScaledTokenEventType.COLLATERAL_BURN
    and ev.user_address == user
    and self._get_reserve_by_token(ev.event["address"]) == withdraw_reserve
    for ev in scaled_events
)

if has_burn_for_asset:
    # There's a burn, so this Mint must be interest accrual
    continue
```

## Transaction Details

- **Hash:** 0xbc0501df0c53d9c449f99bc4ea25f6a809f2bf69a3a530999b9eeab37c079e4f
- **Block:** 23239173
- **Type:** Multi-call via Gnosis Safe
- **User:** 0xF20b338752976878754518183873602902360704

## Key Insight

> **The fundamental issue is not distinguishing between "interest accrual before withdrawal" and "interest exceeds withdrawal."**

The real issue is **cross-asset matching without validation**. The code must verify that:
1. Mint/Burn events match the same asset as the Withdraw operation
2. For multi-asset transactions, events are properly segregated by asset

**This is a broader architectural issue:** Any operation matching logic that associates scaled token events with pool events must verify asset compatibility.

## Related Issues

This pattern may exist in other operation creation functions:
- `_create_supply_operation()`
- `_create_borrow_operation()`
- `_create_repay_operation()`
- `_create_liquidation_operation()`

Each should be reviewed for similar cross-asset matching bugs.

## Testing Considerations

Test cases needed:
1. Multi-asset withdrawal in single transaction (this bug)
2. Interest accrual on Asset A + withdrawal on Asset B
3. Standard interest-exceeds-withdrawal (single asset)
4. Standard withdrawal with burn (single asset)
5. Multiple users withdrawing different assets simultaneously

## Files to Modify

1. `src/degenbot/cli/aave_transaction_operations.py`
   - `_create_withdraw_operation()` - Add asset validation
   - Pattern matching logic around lines 1298-1310
   - Consider adding `_get_reserve_by_token()` helper

2. Potentially other `_create_*_operation()` methods
   - Review for similar cross-asset matching issues

## Fix Implemented

**Status:** ✅ RESOLVED

**Commit:** Modified `_create_withdraw_operation()` in `src/degenbot/cli/aave_transaction_operations.py`

**Changes Made:**

1. **Removed `@staticmethod` decorator** (line 1217) - Changed to instance method to access `self`

2. **Added `self` parameter** (line 1218) - First parameter for instance method

3. **Added asset validation for Burn matching** (lines 1243-1256):
   ```python
   # Get the reserve (underlying asset) from the Withdraw event
   withdraw_reserve = decode_address(withdraw_event["topics"][1])

   # Get the aToken address for this reserve
   expected_a_token = self._get_a_token_for_asset(withdraw_reserve)
   assert expected_a_token is not None, (
       f"Could not find aToken for reserve {withdraw_reserve} in market {self.market.id}"
   )

   # Added check in burn matching loop:
   event_token = get_checksum_address(ev.event["address"])
   if event_token != expected_a_token:
       continue
   ```

4. **Added asset validation for Mint matching** (lines 1294-1297):
   ```python
   # Verify event is from the correct aToken contract
   event_token = get_checksum_address(ev.event["address"])
   if event_token != expected_a_token:
       continue
   ```

5. **Added asset validation for Transfer matching** (lines 1356-1358 and 1372-1374):
   ```python
   # Verify event is from the correct aToken contract
   event_token = get_checksum_address(ev.event["address"])
   if event_token != expected_a_token:
       continue
   ```

### Verification

After fix, the transaction processes correctly:
- Operation 0: WITHDRAW - Uses USDS Burn (logIndex 458) ✓
- Operation 1: WITHDRAW - Uses USDT Mint (logIndex 463, interest exceeds withdrawal) ✓
- USDS Mint at logIndex 444: Correctly becomes INTEREST_ACCRUAL operation (not matched to USDT Withdraw)

Balance verification now passes:
```
AaveV3Market successfully updated to block 23,239,173
```

### Pattern 2 Note

As requested, Pattern 2 matching (`amount == balance_increase`) was left unchanged. The asset validation alone is sufficient to prevent cross-asset matching bugs.

## References

- `_create_withdraw_operation()` in aave_transaction_operations.py
- `_get_a_token_for_asset()` helper method
- AGENTS.md - Interest Accrual section
- Aave V3 aToken contract _transfer function (rev_1.sol:2825-2855)
