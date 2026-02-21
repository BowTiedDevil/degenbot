---
title: GHO Debt Burn Fails to Match REPAY Event Due to max_log_index Constraint
date: Sat Feb 21 2026
category: aave
tags:
  - GHO
  - event-matching
  - max_log_index
  - event-order
related_files:
  - ../../src/degenbot/cli/aave.py
  - ../../src/degenbot/cli/aave_event_matching.py
complexity: simple
---

# GHO Debt Burn Fails to Match REPAY Event Due to max_log_index Constraint

## Symptom

Error message verbatim:
```
ValueError: No matching REPAY/LIQUIDATION_CALL event found for GHO debt burn in tx 
0x6bde612c958454ffc86fd2a4ed59ddd63906ef0dc21320ec41b52661193b0205. 
User: 0xB17bC7ad0E0f73Db0DfE60e508445C237832A369, 
Reserve: 0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f
```

## Root Cause

The GHO_DEBT_BURN event occurs at log index 179, but the matching REPAY event occurs at log index 184. The matching code uses `max_log_index=context.event["logIndex"]`, which limits pool event matching to events with logIndex <= 179, causing the REPAY event at 184 to be skipped.

This happens because Aave V3 emits the Burn event **during** Pool.repay() execution (as an internal call to debtToken.burn()), not after it completes. The event order is:

1. User calls Pool.repay()
2. Pool internally calls debtToken.burn() → emits Burn event (logIndex 179)
3. Pool continues processing → emits Repay event (logIndex 184)

## Transaction Details

- **Hash**: 0x6bde612c958454ffc86fd2a4ed59ddd63906ef0dc21320ec41b52661193b0205
- **Block**: 17699406 (Ethereum mainnet)
- **Type**: GHO debt repayment via Pool.repay()
- **User**: 0xB17bC7ad0E0f73Db0DfE60e508445C237832A369
- **Asset**: 0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f (GHO)

## Event Sequence

| LogIndex | Event | Contract | Details |
|----------|-------|----------|---------|
| 179 | Burn (SCALED_TOKEN_BURN) | GHO Variable Debt Token | from: 0xB17bC..., target: 0x0 |
| 180 | ReserveDataUpdated | Aave V3 Pool | Accrues interest |
| 184 | Repay (REPAY) | Aave V3 Pool | reserve: GHO, user: 0xB17bC... |

## Fix

**File**: `src/degenbot/cli/aave.py`, line 3654

**Change**: Remove `max_log_index` parameter from `find_matching_pool_event()` call for GHO_DEBT_BURN.

```python
# BEFORE (BROKEN):
result = matcher.find_matching_pool_event(
    event_type=ScaledTokenEventType.GHO_DEBT_BURN,
    user_address=user.address,
    reserve_address=reserve_address,
    max_log_index=context.event["logIndex"],  # ← PROBLEM
)

# AFTER (FIXED):
result = matcher.find_matching_pool_event(
    event_type=ScaledTokenEventType.GHO_DEBT_BURN,
    user_address=user.address,
    reserve_address=reserve_address,
)
```

**Safety**:
- GHO has no useATokens flag (no shared REPAY events with collateral burns)
- GHO debt burns only occur during Pool.repay() or Pool.liquidationCall()
- These operations emit at most one REPAY event per transaction
- Exact user and reserve matching prevents false positives
- Affects only GHO debt; standard debt (non-GHO) is unaffected

## Key Insight

The `max_log_index` constraint was designed to prevent causality violations (matching an event to a future event), but it doesn't account for Aave V3's internal call pattern where token contracts emit events **during** Pool function execution. For GHO specifically, this pattern is predictable (burn always during repay), making it safe to remove the constraint.

## Refactoring

Consider the following improvements to code that processes these transactions:

1. **Document internal call patterns**: Add comments explaining which operations emit token events before their pool events
2. **Make max_log_index optional per event type**: Allow certain event types (like GHO burns) to skip the constraint via configuration
3. **Add unit tests for event ordering**: Verify that each scaled token event can correctly match its pool event across all possible orderings

## See Also

- [AGENTS.md](../../../src/degenbot/aave/AGENTS.md) - Aave V3 processor architecture
- [aave_event_matching.py](../../../src/degenbot/cli/aave_event_matching.py) - EventMatcher implementation
- [GhoVariableDebtToken.sol](../../../contract_reference/aave/GhoVariableDebtToken) - GHO contract reference
