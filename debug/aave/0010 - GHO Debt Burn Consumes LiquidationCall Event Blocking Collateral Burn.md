# Aave Debug Progress

## Issue: GHO Debt Burn Consumes LiquidationCall Event, Blocking Collateral Burn

**Date:** 2026-02-20

**Symptom:**
```
ValueError: No matching WITHDRAW, REPAY, or LIQUIDATION_CALL event found for Burn event at block 21908105, logIndex 104
```

**Root Cause:**
In a liquidation transaction involving GHO debt and a non-GHO collateral asset:
1. The GHO debt burn event (logIndex 100) is processed first
2. `_process_gho_debt_burn_event()` matches the LIQUIDATION_CALL event and marks it as consumed
3. The collateral burn event (logIndex 104) is processed next
4. When `_process_collateral_burn_event()` tries to match the LIQUIDATION_CALL event, it finds it already consumed and skips it
5. No other matching pool events are found, causing the error

The bug is in `_process_gho_debt_burn_event()` in `src/degenbot/cli/aave.py` at line 3790:
```python
context.tx_context.matched_pool_events[pool_event_candidate["logIndex"]] = True
```

This always marks the pool event as consumed, regardless of event type. However, LIQUIDATION_CALL events should NOT be consumed because they need to match:
- The GHO debt burn (debt asset = GHO)
- The collateral burn (collateral asset = WETH)

The fix follows the same pattern used in `_process_standard_debt_burn_event()` (lines 3952-3953), which only marks REPAY events as consumed when `useATokens=False`, and never marks LIQUIDATION_CALL events as consumed.

**Transaction Details:**
- **Hash:** 0x574695036709a8a2b5acd4ce82ea6240c256c328042dce48f6b0ea44a3a28445
- **Block:** 21908105
- **Type:** Liquidation
- **User (liquidated):** 0x225c63381cb487f64aa1fc37a59baa3228d6d4ef
- **Liquidator:** 0xe27bfd9d354e7e0f7c5ef2fea0cd9c3af3533a32
- **Collateral Asset:** WETH (0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2)
- **aToken:** aEthWETH (0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8)
- **Debt Asset:** GHO (0x40d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f)
- **vToken:** variableDebtEthGHO (0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B)
- **Events:**
  - Burn at logIndex 100: GHO debt repayment (~39.34 GHO)
  - Burn at logIndex 104: WETH collateral burn (~0.0147 WETH) ← ERROR
  - LiquidationCall at logIndex 113: debtToCover=~39.34 GHO, liquidatedCollateralAmount=~0.0147 WETH

**Fix:**
Modified `_process_gho_debt_burn_event()` in `src/degenbot/cli/aave.py` at lines 3786-3795:

```python
if _matches_pool_event(
    pool_event_candidate, AaveV3Event.REPAY.value, user.address, reserve_address
):
    pool_event = pool_event_candidate
    # Only mark as consumed if NOT a LIQUIDATION_CALL event
    # LIQUIDATION_CALL events should match both debt and collateral burns
    if pool_event_candidate["topics"][0] != AaveV3Event.LIQUIDATION_CALL.value:
        context.tx_context.matched_pool_events[pool_event_candidate["logIndex"]] = True
    break
```

**Key Insight:**
The `_matches_pool_event()` function allows LIQUIDATION_CALL events to match when looking for REPAY events (lines 1404-1408), but different debt burn handlers had inconsistent consumption logic:
- `_process_standard_debt_burn_event()`: Does NOT consume LIQUIDATION_CALL events (correct)
- `_process_gho_debt_burn_event()`: ALWAYS consumes matched events (bug)

This inconsistency meant that liquidations involving GHO debt would fail when there was also a non-GHO collateral asset being liquidated. Pure GHO liquidations (where the collateral is also GHO) would work because both burns would be GHO burns using the same handler.

**Refactoring:**
Consider extracting the event consumption logic into a shared helper function to ensure consistency across all debt burn handlers. The current pattern requires each handler to manually implement the "don't consume LIQUIDATION_CALL events" rule, which is error-prone.

Alternatively, add an assertion in `_process_collateral_burn_event()` that verifies a LIQUIDATION_CALL event exists in `tx_context.pool_events` even if it's already consumed, to provide a clearer error message indicating the root cause (event already consumed by debt burn).
