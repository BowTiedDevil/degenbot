---
title: Collateral Mint Matches Future SUPPLY Event
category: aave
tags:
  - aave
  - event-matching
  - multi-operation
  - bug
complexity: standard
---

# Issue: Collateral Mint Matches Future SUPPLY Event

## Date
2025-02-21

## Symptom
```
No matching event found for COLLATERAL_MINT. Tried users: ['0x6CD71d6Cb7824add7c277F2CA99635D98F8b9248', '0x1809f186D680f239420B56948C58F8DbbCdf1E18'], reserve: 0x7f39C581F595B53c5cb19bD0b3f8da6c935E2Ca0. Available pool events: 3

ValueError: No matching Pool event for collateral mint in tx f1a2cc8ddc3846f93151df903fe63a6603909b468b918185f9b4a6adf0e02e21. User: 0x6CD71d6Cb7824add7c277F2CA99635D98F8b9248, Reserve: 0x7f39C581F595B53c5cb19bD0b3f8da6c935E2Ca0. Available: ['3115d1449a', '2b627736bc', 'a534c8dbe7']
```

## Root Cause

The `EventMatcher.find_matching_pool_event()` method does not enforce chronological ordering when matching scaled token events to pool events. This allows a COLLATERAL_MINT event to match and consume a SUPPLY event that occurs **after** it in the transaction.

In transactions with multiple SUPPLY/COLLATERAL_MINT pairs (common in DeFi aggregators like ParaSwap), this causes the later COLLATERAL_MINT to fail because the SUPPLY event it should match has already been consumed by an earlier COLLATERAL_MINT.

### Transaction Analysis

Transaction: `0xf1a2cc8ddc3846f93151df903fe63a6603909b468b918185f9b4a6adf0e02e21`
Block: 16502006

**Event Sequence:**
| logIndex | Event Type | User | Description |
|----------|-----------|------|-------------|
| 287 | COLLATERAL_MINT | 0x1809f186... (via router) | First aToken mint |
| 293 | WITHDRAW | 0x1809f186... | Router withdraws wstETH |
| 296 | SUPPLY | 0x1809f186... | Router supplies wstETH |
| 317 | COLLATERAL_MINT | 0x6CD71d6... (via router) | Second aToken mint |
| 318 | SUPPLY | 0x6CD71d6... | Actual user supplies wstETH |

**What Happened:**
1. COLLATERAL_MINT at logIndex 287 tries to match a SUPPLY event
2. It finds SUPPLY at logIndex 318 (for user 0x6CD71d6...) before SUPPLY at logIndex 296 (for user 0x1809f186...)
3. The matching logic doesn't check logIndex ordering, so it could match the future SUPPLY
4. SUPPLY at logIndex 318 gets consumed
5. COLLATERAL_MINT at logIndex 317 tries to match but finds SUPPLY at 318 already consumed
6. Error: "No matching Pool event for collateral mint"

## Transaction Details

- **Hash**: `0xf1a2cc8ddc3846f93151df903fe63a6603909b468b918185f9b4a6adf0e02e21`
- **Block**: 16502006
- **Chain**: Ethereum mainnet (chain_id: 1)
- **Transaction Type**: ParaSwap multi-hop swap via Aave
- **Users**: 
  - `0x6CD71d6Cb7824add7c277F2CA99635D98F8b9248` (transaction sender)
  - `0x1809f186D680f239420B56948C58F8DbbCdf1E18` (ParaSwap router)
- **Asset**: wstETH (`0x7f39C581F595B53c5cb19bD0b3f8da6c935E2Ca0`)
- **Pool Events**: 
  - WITHDRAW at logIndex 293
  - SUPPLY at logIndex 296 (for router)
  - SUPPLY at logIndex 318 (for sender)

## Fix

**File**: `src/degenbot/cli/aave_event_matching.py`

### Change 1: Add max_log_index parameter to find_matching_pool_event

```python
def find_matching_pool_event(
    self,
    event_type: ScaledTokenEventType,
    user_address: ChecksumAddress,
    reserve_address: ChecksumAddress,
    *,
    check_users: list[ChecksumAddress] | None = None,
    max_log_index: int | None = None,  # <-- ADDED
) -> EventMatchResult | None:
    """Find a matching pool event for a scaled token event.
    
    Args:
        ...
        max_log_index: Optional maximum logIndex for pool events. Pool events with
            logIndex > max_log_index will be skipped. This prevents matching a
            scaled token event to a pool event that occurs later in the transaction.
    """
```

### Change 2: Filter events by max_log_index in the matching loop

```python
for pool_event in events_of_type:
    # Skip events that occur after the scaled token event
    if max_log_index is not None and pool_event["logIndex"] > max_log_index:
        continue
    
    if self._is_consumed(pool_event):
        # ... rest of the logic
```

### Change 3: Update all call sites to pass event["logIndex"]

Update all 7 call sites in `aave.py` to pass `max_log_index=event["logIndex"]`:
- `_process_collateral_mint_event` (2 call sites)
- `_process_gho_debt_mint_event` (1 call site)
- `_process_standard_debt_mint_event` (1 call site)
- `_process_collateral_burn_event` (1 call site)
- `_process_gho_debt_burn_event` (1 call site)
- `_process_standard_debt_burn_event` (1 call site)

**Rationale**: In Aave V3, the Pool contract emits SUPPLY/WITHDRAW/BORROW/REPAY events BEFORE the scaled token contract emits Mint/Burn events. This is because:
1. User calls `Pool.supply()`
2. Pool emits SUPPLY event
3. Pool calls `aToken.mint()` on the scaled token contract
4. Scaled token emits Mint event

Therefore, a scaled token event should ONLY match pool events with `logIndex <=` its own logIndex.

## Key Insight

Multi-operation transactions involving Aave are common in DeFi aggregators. When a user executes a complex swap through ParaSwap or similar protocols, the transaction may include:
- Multiple WITHDRAW operations to free up collateral
- Multiple SWAP operations through DEXs
- Multiple SUPPLY operations to deposit the output tokens

Each SUPPLY creates a corresponding COLLATERAL_MINT, and the event matcher must correctly pair them without cross-matching between different operations.

## Refactoring

1. **Consider adding transaction pattern detection** to identify multi-operation transactions and handle them specially

2. **Add unit tests** for multi-operation transaction scenarios to prevent regressions

3. **Document the logIndex constraint** in the event matching module to help future developers understand why this ordering is required

4. **Consider adding amount-based matching** as an additional safeguard - only match events if the amounts align (scaled amount from token event ≈ raw amount from pool event)

## References

- Transaction: [0xf1a2cc8ddc3846f93151df903fe63a6603909b468b918185f9b4a6adf0e02e21](https://etherscan.io/tx/0xf1a2cc8ddc3846f93151df903fe63a6603909b468b918185f9b4a6adf0e02e21)
- Block: [16502006](https://etherscan.io/block/16502006)
- Related Issues: Similar to multi-operation issues in #0008, #0012
