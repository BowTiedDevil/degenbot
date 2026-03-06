# 0028 - NoneType Comparison in Repay Operation

**Issue:** NoneType comparison error during REPAY operation processing

**Date:** 2026-03-05

## Symptom

```
TypeError: '>' not supported between instances of 'NoneType' and 'int'
File "/home/ralph/code/degenbot/src/degenbot/cli/aave_transaction_operations.py", line 1079, in _create_repay_operation
    ev.index > 0,
```

## Root Cause

When processing a REPAY_WITH_ATOKENS operation, the code checks for matching BalanceTransfer events by comparing `ev.index > 0`. However, ERC20 Transfer events (which are categorized as COLLATERAL_TRANSFER) have `index=None` because they don't have a liquidity index field like Aave-specific events do.

The `ScaledTokenEvent` dataclass was updated to properly reflect that ERC20 Transfer events don't have these Aave-specific fields by setting `balance_increase` and `index` to `None`, but the comparison code wasn't updated to handle the None case.

## Transaction Details

- **Hash:** 0xa4ee92400377ca7197961c1d5dc6b6738b9474d809206dddbd820641379d3bbc
- **Block:** 16498792
- **Type:** Repay with aTokens (via DefiSaver)
- **User:** 0x21e9f6af91bd687a4856a7dbe4d7f59e9be275f2
- **Asset:** wstETH (aToken: 0x0B925eD163218f6662a35e0f0371Ac234f9E9371)

## Fix

**File:** `src/degenbot/cli/aave_transaction_operations.py`

**Changes:**

1. **Line 1079** (REPAY operation): Added null check before comparison
   ```python
   # Before:
   ev.index > 0,
   
   # After:
   ev.index is not None and ev.index > 0,
   ```

2. **Line 1180** (LIQUIDATION operation): Added null check before comparison
   ```python
   # Before:
   if transfer.index > 0:
   
   # After:
   if transfer.index is not None and transfer.index > 0:
   ```

3. **Line 1347** (Interest accrual processing): Added null guard
   ```python
   # Added before the comparison:
   if ev.balance_increase is None:
       continue
   ```

4. **Line 3360** (Collateral transfer processing): Updated check for ERC20 transfers
   ```python
   # Before:
   if scaled_event.index == 0 and scaled_event.target_address == ZERO_ADDRESS and tx_context:
   
   # After:
   if scaled_event.index is None and scaled_event.target_address == ZERO_ADDRESS and tx_context:
   ```

## Key Insight

When ERC20 Transfer events are decoded as COLLATERAL_TRANSFER events, they don't have Aave-specific fields like `index` and `balance_increase`. These should be `None` to properly reflect the event structure. All comparison code must handle the None case explicitly.

## Refactoring

1. **Type Safety:** The `ScaledTokenEvent` dataclass now properly uses `int | None` for `balance_increase` and `index` to reflect that ERC20 Transfer events don't have these fields.

2. **Null Checks:** All comparison operations on optional fields now include explicit null checks to prevent TypeErrors.

3. **Consistency:** The check for ERC20 Transfer events (which don't have an index) now consistently uses `index is None` instead of `index == 0`.

## Test

Added unit test in `tests/aave/test_repay_with_atokens.py` to verify that:
- REPAY_WITH_ATOKENS operations are correctly parsed
- COLLATERAL_BURN events are properly matched to repay operations
- No TypeError occurs when processing transactions with ERC20 Transfer events

## Related Files

- `src/degenbot/cli/aave_transaction_operations.py` - Main fix location
- `src/degenbot/cli/aave.py` - Collateral transfer processing fix
- `tests/aave/test_repay_with_atokens.py` - Unit tests
