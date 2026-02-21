# Aave Debug Progress

## Issue: Collateral Mint Events Miss LiquidationCall Matching

**Date:** 2025-02-20

**Symptom:** 
```
AssertionError: No matching Pool event for collateral mint in tx 653fcf7f53cc7ce63dd72e2d01f30e3daf9c17095fd8ba1faada2669a959e756. User: 0x8a643B83fE7C75c40f31d6b0d4D494a08FC08d48, Reserve: 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2. Available: ['e413a321e8', 'e413a321e8', 'e413a321e8', 'e413a321e8', 'e413a321e8']
```

**Root Cause:** 
In `_process_standard_debt_mint_event()`, when matching a debt mint event against a LIQUIDATION_CALL pool event (for the self-liquidation case), the code was unconditionally marking the LIQUIDATION_CALL event as consumed. This prevented `_process_collateral_mint_event` from later matching against the same LIQUIDATION_CALL event.

The transaction contained a self-liquidation where the liquidator (0x8a643B83fE7C75c40f31d6b0d4D494a08FC08d48) was also the liquidated user. The flow was:
1. Process debt mint for liquidator → matches LIQUIDATION_CALL at logIndex 143 → marks it as consumed
2. Process collateral mint for liquidator → LIQUIDATION_CALL at logIndex 143 already marked as consumed → no matching event found → error

**Transaction Details:**
- **Hash:** 0x653fcf7f53cc7ce63dd72e2d01f30e3daf9c17095fd8ba1faada2669a959e756
- **Block:** 21921750
- **Type:** Batch Liquidation (5 liquidations in one transaction)
- **Liquidator:** 0x8a643B83fE7C75c40f31d6b0d4D494a08FC08d48 (receives collateral aTokens)
- **Liquidated Users:** 0x45bC0f914C6f9285F41920a2d7D8732743D40474, 0x6ae08ACfeFC524986550224Aa1b07D35E0245d36, 0xf171608cf7db3a629e4232fbb34e722ed1a45f75, 0x2a7d55b86dce425ac1ce2815972c5e742760f14e
- **Self-liquidation:** The liquidator also liquidated their own position at logIndex 143
- **Collateral Asset:** WETH (0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2)
- **Available Pool Events:** 5 LIQUIDATION_CALL events (topic: 0xe413a321e8...)

**Fix:**
One change in `src/degenbot/cli/aave.py`:

**Don't mark LIQUIDATION_CALL as consumed in `_process_standard_debt_mint_event()`** (lines 3435-3442):
```python
if _matches_pool_event(
    pool_event_candidate, expected_type, check_user, reserve_address
):
    pool_event = pool_event_candidate
    # Only mark as consumed if NOT a LIQUIDATION_CALL event
    # LIQUIDATION_CALL events should match both debt mint and collateral mint
    if pool_event_candidate["topics"][0] != AaveV3Event.LIQUIDATION_CALL.value:
        tx_context.matched_pool_events[pool_event_candidate["logIndex"]] = True
    break
```

**Key Insight:** 
The self-liquidation case exposed a bug where LIQUIDATION_CALL events were being marked as consumed by debt mint processing, preventing collateral mint processing from matching them. This is similar to how other functions already handle LIQUIDATION_CALL events (e.g., `_process_collateral_burn_event`, `_process_gho_debt_burn_event`), which don't mark them as consumed.

**Refactoring:**
Consider extracting the "mark as consumed if not LIQUIDATION_CALL" logic into a shared helper function to ensure consistency across all event processing functions.
