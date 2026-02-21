## Issue: GHO Debt Burn Events Fail to Match During Deficit-Created Liquidations

**Date:** 2026-02-20

**Symptom:** 
```
ValueError: No matching REPAY/LIQUIDATION_CALL event found for GHO debt burn in tx 0affc26fff867c734add4067a257ce189f0188aa5c9783489311a5edbb56c306. User: 0xfb2788b2A3a0242429fd9EE2b151e149e3b244eC, Reserve: 0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f
```

**Root Cause:**
GHO (Aave's native stablecoin) uses a different liquidation mechanism than standard Aave assets. While standard assets emit `LiquidationCall` events during liquidations, GHO liquidations emit `DeficitCreated` events instead. The event matching logic had multiple issues:

1. **Wrong event signature**: The `DEFICIT_CREATED` event signature was incorrect (`0x2bcc7b2b...` instead of `0x2bccfb3f...`)
2. **Event not fetched**: The `_fetch_pool_events()` function wasn't fetching `DeficitCreated` events
3. **Event not categorized**: The `_build_transaction_contexts()` function wasn't categorizing `DeficitCreated` events into `pool_events`
4. **Event not matched**: The `EventMatcher` wasn't configured to match GHO debt burns against `DeficitCreated` events

**Transaction Details:**
- **Hash:** 0x0affc26fff867c734add4067a257ce189f0188aa5c9783489311a5edbb56c306
- **Block:** 22127030
- **Type:** Multi-asset liquidation
- **User:** 0xfb2788b2A3a0242429fd9EE2b151e149e3b244eC
- **Assets:** USDC collateral, WBTC debt (standard liquidation), GHO debt (deficit liquidation)

**Event Flow:**
1. Log 660-661: GHO Debt Token Burn (~1.28 GHO)
2. Log 662: DeficitCreated event for GHO
3. Log 664: LiquidationCall event for USDC/WBTC

The GHO debt burn (log 661) should match the DeficitCreated event (log 662), but the code was only looking for REPAY or LIQUIDATION_CALL events.

**Fix:**
Four changes across two files:

**File: `src/degenbot/cli/aave_event_matching.py`**

1. **Corrected DEFICIT_CREATED event signature** (line 111):
   ```python
   # Before:
   DEFICIT_CREATED = HexBytes("0x2bcc7b2bb2ba85c2e6384e005b16394960f7a4c3985ca87f3533247744e0ec12")
   # After:
   DEFICIT_CREATED = HexBytes("0x2bccfb3fad376d59d7accf970515eb77b2f27b082c90ed0fb15583dd5a942699")
   ```

2. **Updated GHO_DEBT_BURN config** (lines 239-252): Added `AaveV3Event.DEFICIT_CREATED` to `pool_event_types` and changed from `REUSABLE` to `CONDITIONAL` consumption policy.

3. **Added DEFICIT_CREATED handler** in `_matches_pool_event` method (lines 395-399): Added a new elif branch to handle matching when `expected_type == AaveV3Event.DEFICIT_CREATED`.

4. **Updated consumption logic** in `_should_consume_gho_debt_burn_pool_event` (lines 708-714): Added `DEFICIT_CREATED` to the set of non-consumable events.

**File: `src/degenbot/cli/aave.py`**

5. **Added DEFICIT_CREATED to event fetching** (lines 4130-4140): Added `AaveV3Event.DEFICIT_CREATED.value` to the topic signature list in `_fetch_pool_events()`.

6. **Added DEFICIT_CREATED to event categorization** (lines 4300-4307): Added `AaveV3Event.DEFICIT_CREATED.value` to the pool_events categorization in `_build_transaction_contexts()`.

**Key Insight:**
When adding support for a new event type, it must be added in multiple places:
1. Event signature constant definition
2. Event fetching (RPC filter)
3. Event categorization (transaction context building)
4. Event matching logic (EventMatcher config)
5. Event consumption logic (if applicable)

Missing any of these steps will cause event processing to fail silently or with cryptic errors.

**Refactoring:**
The event matching framework is well-designed with its declarative configuration pattern. However, the need to update event signatures in multiple places suggests a consolidation opportunity:

- Single source of truth for event signatures (already done in `AaveV3Event` enum)
- Automatic validation that all defined events are fetched and categorized
- Better error messages when unknown events are encountered

Future improvements could include:
- Asset-specific matching configurations (e.g., GHO vs non-GHO debt burns)
- Better documentation of asset-specific behaviors in the Aave protocol
- Unit tests that cover all event type combinations

**Testing:**
After the fix, the Aave update command completes successfully without the ValueError. The transaction is now processed correctly with the GHO debt burn matching the DeficitCreated event.
