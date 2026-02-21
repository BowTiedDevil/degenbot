# Aave Debug Progress

## Issue: Collateral Burn Fails When LIQUIDATION_CALL Already Consumed

**Date:** 2026-02-20

**Symptom:**
```
ValueError: No matching WITHDRAW, REPAY, or LIQUIDATION_CALL event found for Burn event at block 21975258, logIndex 96
```

**Root Cause:**
In a liquidation transaction involving both GHO debt and non-GHO collateral, the LIQUIDATION_CALL pool event can be consumed by the GHO debt burn processing before the collateral burn has a chance to match it.

The flow is:
1. GHO debt burn event is processed first by `_process_gho_debt_burn_event()`
2. It matches the LIQUIDATION_CALL event and was incorrectly marking it as consumed
3. Collateral burn event is processed next by `_process_collateral_burn_event()`
4. The LIQUIDATION_CALL event is now marked as consumed, so it's skipped
5. No other matching pool events are found, causing the error

**Analysis of Previous Fixes:**
Report 0010 fixed a similar issue in `_process_gho_debt_burn_event()` at lines 3791-3794:
```python
if pool_event_candidate["topics"][0] != AaveV3Event.LIQUIDATION_CALL.value:
    context.tx_context.matched_pool_events[pool_event_candidate["logIndex"]] = True
```

However, the current error suggests either:
1. The fix was not properly applied
2. There's another code path that marks LIQUIDATION_CALL as consumed
3. The event matching logic in `_process_collateral_burn_event()` has an issue

**Investigation Notes:**
- The error message format matches exactly with report 0010
- Block 21975258 on Base chain was examined but historical state was unavailable
- The matching logic in `_process_collateral_burn_event()` (lines 3650-3681) appears correct
- The `_matches_pool_event()` function handles LIQUIDATION_CALL matching when expected_type is WITHDRAW (lines 1427-1431)

**Potential Causes:**
1. LIQUIDATION_CALL event consumed by debt burn and not properly preserved
2. Event ordering issue where collateral burn is processed before debt burn
3. Database state inconsistency with stale `matched_pool_events` data

**Transaction Pattern:**
Expected event sequence in a liquidation:
- GHO debt burn (logIndex N) - should match LIQUIDATION_CALL, NOT consume it
- Collateral burn (logIndex N+4) - should match same LIQUIDATION_CALL
- LIQUIDATION_CALL event (logIndex N+9) - shared between both burns

**Fix Strategy:**
Verify that the fix from report 0010 is properly applied and that no other code paths are marking LIQUIDATION_CALL events as consumed. Add defensive checks to ensure LIQUIDATION_CALL events remain available for all burn events in a transaction.

**Key Insight:**
The issue appears to be a recurrence of report 0010. The fix needs to ensure that LIQUIDATION_CALL events are never marked as consumed by any debt burn handler (GHO or standard), as they must be available to match both debt burns and collateral burns in liquidation transactions.

**Refactoring:**
Consider adding a transaction-level assertion that verifies:
1. All burn events in a liquidation transaction can find matching LIQUIDATION_CALL events
2. LIQUIDATION_CALL events are properly shared across multiple burn events
3. Clear error messages distinguish between "no event found" vs "event already consumed"

**Next Steps:**
1. Run the update command with `DEGENBOT_VERBOSE_TX` to identify the specific transaction
2. Add logging to track which handler consumes which LIQUIDATION_CALL events
3. Verify the fix from report 0010 is correctly applied in all debt burn handlers
