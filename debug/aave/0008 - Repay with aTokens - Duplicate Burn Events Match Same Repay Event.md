# Aave Debug Progress

## Issue: Repay with aTokens - Duplicate Burn Events Match Same Repay Event

**Date:** 2026-02-20

### Symptom
```
ValueError: No matching WITHDRAW or REPAY event found for Burn event at block 21892044, logIndex 100
```

### Root Cause
In a "repay with aTokens" transaction, the user repays debt by burning aTokens directly instead of underlying tokens. This produces TWO burn events:
1. **vToken Burn** (logIndex 97) - reduces debt
2. **aToken Burn** (logIndex 100) - reduces collateral

But there is only **ONE** Repay event (logIndex 101) with `useATokens=True`.

**Transaction Flow (0xc05df665f4bb647b354a5592d34732111526d97c9dd8bbefd32dbc88d3e4605f):**
1. logIndex 97: vToken Burn (debt repayment)
2. logIndex 100: aToken Burn (collateral reduction)
3. logIndex 101: Repay event with `useATokens=True`

The code processed events in order:
- vToken Burn (97) → matched with Repay (101) and **marked it as consumed**
- aToken Burn (100) → searched for Pool event but Repay was already consumed → **ERROR**

**Bug Location:** `_process_standard_debt_burn_event()` in `src/degenbot/cli/aave.py` lines 3901

The original code always marked the Repay event as consumed:
```python
pool_event = pool_event_candidate
tx_context.matched_pool_events[pool_event_candidate["logIndex"]] = True  # Always consumed
break
```

### Fix
Modified `_process_standard_debt_burn_event()` to:
1. Capture the `useATokens` flag from the Repay event data
2. Only mark the Repay as consumed if `useATokens=False`
3. When `useATokens=True`, leave it available for the collateral burn processing

**Code Changes:**
```python
# Decode useATokens flag
(payback_amount, use_a_tokens) = eth_abi.abi.decode(
    types=["uint256", "bool"],
    data=pool_event["data"],
)
# Only mark as consumed if NOT using aTokens
if not use_a_tokens:
    tx_context.matched_pool_events[pool_event["logIndex"]] = True
```

**Files Modified:**
- `src/degenbot/cli/aave.py`

### Transaction Details
- **Hash:** 0xc05df665f4bb647b354a5592d34732111526d97c9dd8bbefd32dbc88d3e4605f
- **Block:** 21892044
- **Type:** repayWithPermit() with useATokens=true
- **User:** 0x4490db0fc0e8de7c7192f12f9c5e8409e7cadda2
- **Asset:** USDC (aEthUSDC collateral + variableDebtEthUSDC debt)
- **Repay Amount:** 25,000 USDC

### Key Insight
The Aave V3 Pool contract's `executeRepay` function supports repaying debt with aTokens (useATokens=true) instead of underlying tokens. When this happens:
- The debt token (vToken) is burned to reduce debt
- The collateral aToken is also burned to reduce collateral
- Both burns are associated with the same Repay event

The fix ensures the Repay event is not marked as consumed when useATokens=true, allowing both burn events to match it.

### Refactoring
Consider creating a dedicated helper function to handle the "repay with aTokens" scenario, making it clear that one Repay event can match two burn events. The current logic is spread across multiple functions and is not immediately obvious.
