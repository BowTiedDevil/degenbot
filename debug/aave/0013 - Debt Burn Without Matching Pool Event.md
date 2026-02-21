# Aave Debug Progress

## Issue: Debt Burn Without Matching Pool Event

**Date:** 2026-02-20

### Symptom
```
ValueError: No matching REPAY event found for debt burn in tx 0x5e1a466b9d5618d83f85d706b467627116726f9924c4e4a50a4e89a0282b8012. User: 0x152356d19068C0F65cAB4Ecb759236Bb0865A932, Reserve: 0x514910771AF9Ca656af840dff83E8264EcF986CA
```

### Root Cause
In a flash loan liquidation transaction, the LINK debt token burn has **no corresponding Pool event** (REPAY, LIQUIDATION_CALL, or DEFICIT_CREATED).

**Transaction Flow (0x5e1a466b9d5618d83f85d706b467627116726f9924c4e4a50a4e89a0282b8012):**
1. logIndex 104: USDC vToken Burn - matches DeficitCreated event (bad debt write-off)
2. logIndex 109: WETH aToken Burn - matches LiquidationCall event
3. **logIndex 116: LINK vToken Burn - NO MATCHING POOL EVENT**

The code in `_process_standard_debt_burn_event()` expected every debt burn to have a matching Pool event, but this flash loan liquidation burns LINK debt directly without emitting a Pool-level event.

This is an edge case that occurs in complex liquidation flows where debt tokens are burned directly without going through the Pool's `repay()` or `liquidationCall()` functions. Possible causes:
- Protocol-level debt forgiveness or bad debt write-off
- Direct vToken contract interaction outside the Pool contract
- Complex liquidation mechanics in flash loan transactions

**Bug Location:** `_process_standard_debt_burn_event()` in `src/degenbot/cli/aave.py` lines 3946-3951

The original code raised a ValueError when no matching Pool event was found:
```python
if pool_event is None:
    msg = (
        f"No matching REPAY event found for debt burn in tx {tx_context.tx_hash.hex()}. "
        f"User: {user.address}, Reserve: {reserve_address}"
    )
    raise ValueError(msg)
```

### Fix
Modified `_process_standard_debt_burn_event()` to handle debt burns without matching Pool events:

1. When no Pool event is found, log a warning and use the event_amount directly as the scaled amount
2. The event_amount from the Burn event is already the scaled amount (vToken balance units)
3. Continue processing the debt position update normally

**Code Changes:**
```python
if pool_event is None:
    # Edge case: debt burn without matching Pool event
    # This can occur in flash loan liquidations, protocol upgrades, or bad debt forgiveness
    # where the debt token is burned directly without going through Pool.repay()
    logger.warning(
        f"No matching REPAY/LIQUIDATION_CALL/DEFICIT_CREATED event found for debt burn "
        f"in tx {tx_context.tx_hash.hex()}. User: {user.address}, Reserve: {reserve_address}. "
        f"Processing debt burn using event data directly."
    )
    # When no Pool event exists, the event_amount from the Burn event is already
    # the scaled amount
    scaled_amount = event_amount
else:
    # Original logic for extracting paybackAmount from Pool event
    ...
```

**Files Modified:**
- `src/degenbot/cli/aave.py`

### Transaction Details
- **Hash:** 0x5e1a466b9d5618d83f85d706b467627116726f9924c4e4a50a4e89a0282b8012
- **Block:** 21990197
- **Type:** Flash loan liquidation with bad debt write-off
- **User (liquidated):** 0x152356d19068C0F65cAB4Ecb759236Bb0865A932
- **Liquidator:** 0x04804e6a704f70b2e2aea1edece51c2b53c6b05c (flash loan contract)
- **Assets:**
  - USDC debt (0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48) - 11,347,979 units via DeficitCreated
  - WETH collateral (0xC02aaA39b223FE8D0A0E5C4F27eAD9083C756Cc2) - ~0.083 WETH via LiquidationCall
  - LINK debt (0x514910771AF9Ca656af840dff83E8264EcF986CA) - ~340,456 units burned without Pool event

### Key Insight
The Aave V3 protocol has edge cases where debt tokens can be burned outside the normal Pool contract flow. When this happens:
- The Burn event is still valid and should be processed
- The vToken contract emits the Burn event with the scaled amount
- No Pool event is emitted since the Pool contract wasn't involved
- The debt reduction is still legitimate and should be reflected in user positions

The fix ensures that debt burns are processed even when no Pool event exists, by using the scaled amount directly from the Burn event data.

### Refactoring
Consider adding a configuration option to control whether debt burns without Pool events should be:
1. Processed with a warning (current behavior)
2. Processed silently
3. Rejected (for strict mode)

This would allow operators to choose their preferred behavior based on their risk tolerance for edge cases.

Additionally, consider tracking these "orphan" debt burns in a separate table for audit purposes, to identify patterns in protocol-level debt adjustments.
