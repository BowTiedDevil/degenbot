# Aave Debug Progress

## Issue: Pure Interest Mint Incorrectly Matches SUPPLY Event

**Date:** 2025-02-21

**Symptom:**
```
AssertionError: User 0x6CD71d6Cb7824add7c277F2CA99635D98F8b9248: collateral balance (155115929661084444440) does not match scaled token contract (155050074056342129700) @ 0x0B925eD163218f6662a35e0f0371Ac234f9E9371 at block 16502006
```

## Root Cause

When processing a Mint event where `value == balance_increase` (pure interest accrual), the code was incorrectly matching it to a SUPPLY event from later in the transaction. This caused the interest amount to be added to the balance twice:

1. Once during the Mint event processing (when `scaled_amount` was calculated from the matched SUPPLY)
2. Once more during the actual SUPPLY event processing

The bug was in `_process_collateral_mint_event()` in `src/degenbot/cli/aave.py`. When a Mint event with `value == balance_increase` matched a SUPPLY event, it would calculate a `scaled_amount` from that SUPPLY and pass it to the processor. The processor would then add this amount to the balance, even though `value == balance_increase` should mean pure interest accrual with `balance_delta = 0`.

## Transaction Details

- **Hash:** 0xf1a2cc8ddc3846f93151df903fe63a6603909b468b918185f9b4a6adf0e02e21
- **Block:** 16502006
- **Type:** swapAndRepay via ParaSwap adapter
- **User:** 0x6CD71d6Cb7824add7c277F2CA99635D98F8b9248
- **Asset:** AwstETH (0x0B925eD163218f6662a35e0f0371Ac234f9E9371)

### Event Sequence

| logIndex | Event | User | Amount |
|----------|-------|------|--------|
| 287 | Mint | 0x6CD71d... | value=balanceIncrease (65,855,604,742,314,740) - pure interest |
| 290 | BalanceTransfer | 0x6CD71d... → 0x1809f186... | 170,694,594,781,963,877,788 |
| 294 | Withdraw | 0x1809f186... | 170,694,594,781,963,877,788 |
| 317 | Mint | 0x6CD71d... | value=balanceIncrease (65,855,604,742,314,740) - deposit |

The Mint at log 287 is pure interest accrual (value == balance_increase), but it was incorrectly matching to the SUPPLY event implied by log 317, causing the interest amount to be added to the balance.

## Fix

### File: `src/degenbot/cli/aave.py`

Added validation in `_process_collateral_mint_event()` to check that when `value == balance_increase`, the calculated scaled amount from a matched SUPPLY event equals the event value. If not, the SUPPLY event doesn't match this Mint event:

```python
# When value == balance_increase, validate that the calculated scaled amount
# equals the event value. If not, this SUPPLY event doesn't match this Mint
# event (e.g., the Mint is pure interest accrual before a transfer, not a deposit).
# This prevents incorrectly matching unrelated SUPPLY events.
# ref: Bug #0024
if event_amount == balance_increase and calculated_scaled_amount != event_amount:
    # This is not a matching SUPPLY event - it's pure interest accrual
    matched_pool_event = None
    scaled_amount = None
else:
    scaled_amount = calculated_scaled_amount
```

## Key Insight

When `value == balance_increase` in a Mint event, it can mean either:
1. **Pure interest accrual** (no balance change, just index update)
2. **Deposit where deposit amount equals accrued interest** (balance increases by scaled_amount)

The distinction is whether there's a corresponding SUPPLY event with matching amounts. The fix validates that the SUPPLY event's scaled amount equals the Mint event's value before accepting the match.

## Verification

- Aave update now processes block 16502006 successfully
- Verification passes for user 0x6CD71d6Cb7824add7c277F2CA99635D98F8b9248
- Final balance matches contract: 155050074056342129700

## Refactoring

Consider adding a more robust validation framework for event matching that:
1. Validates amount relationships between scaled token events and pool events
2. Checks temporal ordering (e.g., SUPPLY events should come after their corresponding Mint events)
3. Validates user address consistency across matched events

This would prevent similar bugs where unrelated events are incorrectly matched due to address overlap in the same transaction.
