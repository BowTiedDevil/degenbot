# Aave Debug Progress

## Issue: Collateral Operations Consume LIQUIDATION_CALL and REPAY Events

**Date:** 2026-02-20

**Symptom:**
```
ValueError: No matching WITHDRAW, REPAY, or LIQUIDATION_CALL event found for Burn event at block 21975258, logIndex 96. User: 0x65c748e146D83C189D9aF9EB0E98a657898633DA, Reserve: 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48.
```

**Root Cause:**
In a "repay with aTokens" transaction, the following events occur:
1. Mint event at logIndex 93 (interest accrual on collateral)
2. Burn event at logIndex 96 (burning aTokens to repay)
3. REPAY pool event at logIndex 98

The `_process_standard_debt_mint_event()` function was consuming the REPAY event, preventing the collateral burn from matching it. The issue was that the function only checked for LIQUIDATION_CALL before marking events as consumed, but not for REPAY.

**Transaction Details:**
- **Hash:** 0x103d7a70ddece0223a848bf5fd7f0c2df6c683b5f2bd7a96f1bc48aadcab7335
- **Block:** 21975258 (Base chain)
- **Type:** Repay with aTokens
- **User:** 0x65c748e146D83C189D9aF9EB0E98a657898633DA
- **Collateral Asset:** USDC (0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48)
- **Events:**
  - Mint at logIndex 93: Interest accrual
  - Burn at logIndex 96: aUSDC burn to repay
  - REPAY at logIndex 98: Pool repay event

**Fix:**
Modified `_process_standard_debt_mint_event()` in `src/degenbot/cli/aave.py` to not consume REPAY events:

```python
# Only mark as consumed if NOT a LIQUIDATION_CALL or REPAY event
# LIQUIDATION_CALL events should match multiple operations in liquidations
# REPAY events should match debt burns (for repay with aTokens)
event_topic = pool_event_candidate["topics"][0]
if event_topic not in {
    AaveV3Event.LIQUIDATION_CALL.value,
    AaveV3Event.REPAY.value,
}:
    tx_context.matched_pool_events[pool_event_candidate["logIndex"]] = True
```

Also removed REPAY from the match_sequence in `_process_collateral_mint_event()` (lines 3102-3114) to prevent collateral mints from matching REPAY events inappropriately.

**Additional Fixes:**
1. Removed REPAY from match_sequence in `_process_collateral_mint_event()`
2. Added check to not consume REPAY in `_process_standard_debt_mint_event()`
3. Already had checks to not consume LIQUIDATION_CALL in various handlers

**Files Modified:**
- `src/degenbot/cli/aave.py` (lines 3102-3114, 3456-3464)

**Test:**
Created `tests/cli/test_aave_liquidation_event_matching.py` to verify the consumption patterns.

**Key Insight:**
Event consumption must be carefully managed to ensure:
1. LIQUIDATION_CALL events are never consumed (shared across multiple operations)
2. REPAY events are only consumed by burn operations (for repay with aTokens)
3. Other events (SUPPLY, WITHDRAW, BORROW) can be consumed normally

**Prevention:**
To prevent similar bugs:
1. Always document which event types can match multiple operations
2. Review event consumption logic when adding new handlers
3. Consider a shared helper function for marking events as consumed
