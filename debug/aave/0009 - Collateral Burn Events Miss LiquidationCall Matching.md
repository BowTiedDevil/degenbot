# Aave Debug Progress

## Issue: Collateral Burn Events Miss LiquidationCall Matching

**Date:** 2025-02-20

**Symptom:** 
```
ValueError: No matching WITHDRAW or REPAY event found for Burn event at block 21893775, logIndex 409
```

**Root Cause:** 
The `_process_collateral_burn_event()` function in `src/degenbot/cli/aave.py` only checked for `WITHDRAW` and `REPAY` events when matching a collateral (aToken) Burn event. However, during liquidations, the Aave Pool emits a `LiquidationCall` event instead of `WITHDRAW` or `REPAY` when burning aTokens to seize collateral.

The matching loop was:
```python
for pool_event_candidate in tx_context.pool_events:
    event_topic = pool_event_candidate["topics"][0]
    
    if event_topic == AaveV3Event.WITHDRAW.value:
        # Match withdrawal...
    elif event_topic == AaveV3Event.REPAY.value:
        # Match repayment...
    # Missing: LIQUIDATION_CALL check!
```

Since `LiquidationCall` events were never iterated over, they could never be matched, causing the error when processing collateral burns from liquidations.

**Transaction Details:**
- **Hash:** 0x8a843f0cf626d6e972c144a4b3d2fc920126f8630749f12167322886df6ee825
- **Block:** 21893775
- **Type:** Liquidation
- **User (liquidated):** 0xaca98ec16bf9174c6acb486870bab8616d1e5a3b
- **Liquidator:** 0x9d6b911199b891c55a93e4bc635bf59e33d002d8
- **Collateral Asset:** cbBTC (0xcbb7c0000ab88b473b1f5afd9ef808440eed33bf)
- **aToken:** aEthcbBTC (0x5c647ce0ae10658ec44fa4e11a51c96e94efd1dd)
- **Debt Asset:** WETH
- **Events:** 
  - Burn at logIndex 409: value=226,483 units (collateral seized)
  - LiquidationCall at logIndex 416: debtToCover=~0.0118 WETH

**Fix:**
Added `LIQUIDATION_CALL` event matching in `_process_collateral_burn_event()` in `src/degenbot/cli/aave.py`:

```python
# Check for LIQUIDATION_CALL (collateral seized during liquidation)
elif event_topic == AaveV3Event.LIQUIDATION_CALL.value and _matches_pool_event(
    pool_event_candidate, AaveV3Event.WITHDRAW.value, user.address, reserve_address
):
    pool_event = pool_event_candidate
    # Note: LIQUIDATION_CALL events are not marked as consumed here
    # because they may match multiple burn events (both debt and collateral)
    break
```

Also updated the error message and added handling for extracting `liquidatedCollateralAmount` from `LIQUIDATION_CALL` events:

```python
elif pool_event_topic == AaveV3Event.LIQUIDATION_CALL.value:
    # LIQUIDATION_CALL event data: (uint256 debtToCover, uint256 liquidatedCollateralAmount,
    #                               address liquidator, bool receiveAToken)
    (_, liquidated_collateral_amount, _, _) = eth_abi.abi.decode(
        types=["uint256", "uint256", "address", "bool"],
        data=pool_event["data"],
    )
    pool_processor = PoolProcessorFactory.get_pool_processor_for_token_revision(
        collateral_asset.a_token_revision
    )
    scaled_amount = pool_processor.calculate_collateral_burn_scaled_amount(
        amount=liquidated_collateral_amount,
        liquidity_index=index,
    )
```

**Key Insight:** 
The `_matches_pool_event()` function already supported matching `LiquidationCall` events when looking for `WITHDRAW` events (lines 1426-1430), but the calling code in `_process_collateral_burn_event()` never actually iterated over `LiquidationCall` events due to the topic checks. This was an integration bug where the matching logic existed but wasn't connected to the event iteration.

**Refactoring:**
Consider extracting pool event iteration logic into a shared helper that can match any supported event type (WITHDRAW, REPAY, LIQUIDATION_CALL, DEFICIT_CREATED) to avoid similar issues in the future. The current pattern requires each burn event processor to manually enumerate event types to check.
