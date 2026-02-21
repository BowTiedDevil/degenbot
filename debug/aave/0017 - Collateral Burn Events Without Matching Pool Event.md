# Aave Debug Progress

## Issue: Collateral Burn Events Without Matching Pool Event

**Date:** 2025-02-21

**Symptom:**
```
ValueError: No matching WITHDRAW, REPAY, or LIQUIDATION_CALL event found for Burn event at block 22638170, logIndex 219
```

**Root Cause:**
The `_process_collateral_burn_event()` function in `src/degenbot/cli/aave.py` expected to always find a matching Pool event (WITHDRAW, REPAY, or LIQUIDATION_CALL) for every collateral (aToken) Burn event. However, some Burn events can occur without a corresponding Pool event, such as:

1. Direct aToken burns to the contract itself (burn to address(0) or contract address)
2. Protocol upgrade operations that burn collateral without going through the Pool
3. Edge cases where collateral is burned as part of complex multi-step operations

The similar function `_process_standard_debt_burn_event()` already handled this edge case (see debug/aave/0013), but `_process_collateral_burn_event()` did not.

**Transaction Details:**
- **Hash:** 0xaa900e1ac9ece8a1a0db38c111ccfe5b5fb735a838278995a7e6534a8fc32a63
- **Block:** 22638170
- **Type:** Direct aToken burn (no Pool event)
- **User:** 0xd400fc38ed4732893174325693a63c30ee3881a8
- **Asset:** USDC (0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48)
- **aToken:** aUSDC (0x98c23e9d8f34fefb1b7bd6a91b7ff122f4e16f5c)
- **Events:**
  - Burn at logIndex 219: value=168401963, balanceIncrease=0, target=aToken contract
  - No WITHDRAW, REPAY, or LIQUIDATION_CALL events for this user/asset

**Analysis:**
The Burn event at logIndex 219 has:
- `from`: 0xd400fc38ed4732893174325693a63c30ee3881a8 (user)
- `target`: 0x98c23e9d8f34fefb1b7bd6a91b7ff122f4e16f5c (aUSDC contract itself)
- `value`: 168401963 (not equal to balanceIncrease, so not pure interest)
- `balanceIncrease`: 0

This appears to be a direct burn of aTokens to the contract, possibly as part of a position cleanup or protocol operation.

**Fix:**
Modified `_process_collateral_burn_event()` in `src/degenbot/cli/aave.py` to handle the case where no matching Pool event exists:

```python
if result is None:
    # Edge case: collateral burn without matching Pool event
    # This can occur in protocol upgrades, direct aToken burns, or other
    # edge cases where the collateral is burned without going through
    # Pool.withdraw(), Pool.repay(), or Pool.liquidationCall()
    # When no Pool event exists, use the event_amount directly
    # The event_amount from the Burn event is already the scaled amount
    scaled_amount = event_amount
else:
    pool_event = result["pool_event"]
    extraction_data = result["extraction_data"]
    pool_event_topic = pool_event["topics"][0]
    
    pool_processor = PoolProcessorFactory.get_pool_processor_for_token_revision(
        collateral_asset.a_token_revision
    )
    
    if pool_event_topic == AaveV3Event.WITHDRAW.value:
        # WITHDRAW event: use raw_amount
        raw_amount = extraction_data["raw_amount"]
        scaled_amount = pool_processor.calculate_collateral_burn_scaled_amount(
            amount=raw_amount,
            liquidity_index=index,
        )
    elif pool_event_topic == AaveV3Event.LIQUIDATION_CALL.value:
        # LIQUIDATION_CALL event: use liquidated_collateral amount
        liquidated_amount = extraction_data["liquidated_collateral"]
        scaled_amount = pool_processor.calculate_collateral_burn_scaled_amount(
            amount=liquidated_amount,
            liquidity_index=index,
        )
    # REPAY events don't provide scaled_amount (handled by debt burn)
```

**Key Insight:**
When no Pool event exists, the Burn event's `value` field is already the scaled amount (unlike WITHDRAW or LIQUIDATION_CALL events where we need to convert the raw amount to scaled amount). This is consistent with the handling in `_process_standard_debt_burn_event()`.

**Prevention:**
To prevent similar bugs:
1. Always consider edge cases where Pool events might not exist
2. Review the debt burn handler when implementing collateral burn handling
3. Look for patterns where `result is None` needs special handling
4. Consider adding comprehensive event logs for debugging unmatched events

**Related Issues:**
- debug/aave/0013 - Debt Burn Without Matching Pool Event (similar fix pattern)
- debug/aave/0008 - Repay with aTokens event matching
- debug/aave/0009 - Collateral Burn Events Miss LiquidationCall Matching
