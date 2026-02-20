# Aave Debug Progress

## Issue: GHO Interest Accrual Misclassified as Repay

**Date:** 2025-02-18

**Symptom:** 
```
AssertionError: User 0x4f7728eab6f5394f03e84dFB953377E707F1108F: debt balance (4559986608923774469920) does not match scaled token contract (4559986530841813789612) @ 0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B at block 17699757
```

**Root Cause:** 
The `GhoV1Processor.process_mint_event()` method in `src/degenbot/aave/processors/debt/gho/v1.py` incorrectly handles the case where `value == balance_increase` in a Mint event. This condition represents **interest accrual** (not a borrow or repay), which occurs when the discount rate is updated and triggers `_accrueDebtOnAction()`. The existing code falls through to the repay logic, incorrectly calculating the balance delta as `discount_scaled - amount_scaled` where `amount_scaled = 0`, resulting in adding `discount_scaled` to the balance instead of subtracting it.

**Transaction Details:**
- **Hash:** 0xbd8f927f12dff559674eeeb53988efe6b7f6cb4f382c796a2c0301e59b851076
- **Block:** 17699757
- **Type:** GHO Interest Accrual (from stkAAVE rewards claim)
- **User:** 0x4f7728eab6f5394f03e84dFB953377E707F1108F
- **Asset:** vGHO (0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B)
- **Event:** Mint with `value = 9191893664374059` and `balance_increase = 9191893664374059`

**Fix:**
Updated `src/degenbot/aave/processors/debt/gho/v1.py` to handle three distinct cases in `process_mint_event()`:

```python
if event_data.value > event_data.balance_increase:
    # GHO BORROW (unchanged)
    ...
elif event_data.balance_increase > event_data.value:
    # GHO REPAY (unchanged)
    ...
else:
    # NEW: Pure interest accrual (value == balance_increase)
    # Emitted from _accrueDebtOnAction during discount updates
    # The balance decreases by the discount amount (burned by contract)
    balance_delta = -discount_scaled
    user_operation = GhoUserOperation.GHO_INTEREST_ACCRUAL
```

**Key Insight:** 
GHO debt tokens emit Mint events in three scenarios:
1. **Borrow:** `value > balance_increase` - new debt being minted
2. **Repay:** `balance_increase > value` - debt being repaid (burned)
3. **Interest Accrual:** `value == balance_increase` - interest added to debt when discount rate changes

The v2.py processor already had this fix, but v1.py was missing the third case. Always check all revision-specific processor implementations when fixing token processing bugs.

**Refactoring:**
Consider extracting the common Mint event processing logic into a shared helper function in the base `GhoDebtTokenProcessor` class to avoid duplication across v1.py, v2.py, and v4.py processors.
