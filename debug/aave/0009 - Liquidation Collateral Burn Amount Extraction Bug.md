# Issue 0009: Liquidation Collateral Burn Amount Extraction Bug

**Date:** 2026-03-15

**Symptom:**
```
AssertionError: Balance verification failure for AaveV3Asset(...). User 0x9644Af7328aa79cBE8DC5882CD2016d56d819058 scaled balance (-6317689281796260) does not match contract balance (0) at block 23089113
```

**Root Cause:**

The `RawAmountExtractor._extract_liquidation()` method in `src/degenbot/aave/extraction.py` only extracts `debtToCover` from the LiquidationCall event. However, liquidation operations have TWO amounts:
1. `debtToCover` - the debt being repaid (for debt burns)
2. `liquidatedCollateralAmount` - the collateral being seized (for collateral burns)

When processing a collateral burn in a LIQUIDATION operation, the enrichment code incorrectly uses `debtToCover` (6,338,549,079,481,902 wei) instead of `liquidatedCollateralAmount` (21,830 satoshis). The TokenMath calculation then produces a wildly incorrect scaled amount:

```python
# Wrong calculation for collateral burn:
scaled_amount = ray_div_ceil(debt_to_cover, liquidity_index)
# = ray_div_ceil(6,338,549,079,481,902, 1003371457925023383111049510)
# ≈ 6,316,768,928,181,8017 (way too large!)

# Correct calculation should be:
scaled_amount = ray_div_ceil(liquidated_collateral_amount, liquidity_index)
# = ray_div_ceil(21,830, 1003371457925023383111049510)
# ≈ 21,757 (correct!)
```

**Transaction Details:**
- **Hash:** 0x71928c08168e75cb6f9382a1fb68d348ae1486aef6885904f922a35e5af6e0e5
- **Block:** 23089113
- **Type:** LIQUIDATION
- **User:** 0x9644Af7328aa79cBE8DC5882CD2016d56d819058
- **Collateral Asset:** WBTC (0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599)
- **Debt Asset:** WETH (0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2)
- **Debt Burn Amount:** 6,338,549,079,481,902 wei (from Burn event)
- **Collateral Burn Amount:** 21,830 satoshis (from Burn event)
- **Contract Revisions:** Pool rev 9, aToken rev 4, vToken rev 4

**Fix:**

Files Modified:
- `src/degenbot/aave/extraction.py` (lines 148-194)
- `src/degenbot/aave/enrichment.py` (lines 9, 127-172)

**Changes Made:**

1. **extraction.py**: Split `_extract_liquidation` into two separate methods:
   - `_extract_liquidation_debt()`: Extracts `debtToCover` (for debt burns)
   - `_extract_liquidation_collateral()`: Extracts `liquidatedCollateralAmount` (for collateral burns)

2. **enrichment.py**: Added special handling for LIQUIDATION operations:
   - Detects when processing a LIQUIDATION operation
   - For debt events (DEBT_BURN, GHO_DEBT_BURN, etc.): uses `_extract_liquidation_debt()`
   - For collateral events (COLLATERAL_BURN, COLLATERAL_TRANSFER, etc.): uses `_extract_liquidation_collateral()`
   - Non-liquidation operations continue to use standard extraction

**Key Code Changes:**

```python
# In extraction.py - split liquidation extractor into two methods
@staticmethod
def _extract_liquidation_debt(event: LogReceipt) -> int:
    """Extract debtToCover from LiquidationCall event."""
    debt_to_cover, _, _, _ = eth_abi.abi.decode(
        types=["uint256", "uint256", "address", "bool"],
        data=event["data"],
    )
    return debt_to_cover

@staticmethod
def _extract_liquidation_collateral(event: LogReceipt) -> int:
    """Extract liquidatedCollateralAmount from LiquidationCall event."""
    _, liquidated_collateral, _, _ = eth_abi.abi.decode(
        types=["uint256", "uint256", "address", "bool"],
        data=event["data"],
    )
    return liquidated_collateral

# In enrichment.py - select correct extractor based on event type
if operation.operation_type.name in {"LIQUIDATION", "GHO_LIQUIDATION", "SELF_LIQUIDATION"}:
    if operation.pool_event["topics"][0] == AaveV3PoolEvent.LIQUIDATION_CALL.value:
        if scaled_event.event_type in {ScaledTokenEventType.DEBT_BURN, ...}:
            raw_amount = RawAmountExtractor._extract_liquidation_debt(operation.pool_event)
        elif scaled_event.event_type in {ScaledTokenEventType.COLLATERAL_BURN, ...}:
            raw_amount = RawAmountExtractor._extract_liquidation_collateral(operation.pool_event)
```

**Key Insight:**

Multi-amount Pool events (like LiquidationCall) require special handling in the enrichment layer. The current design assumes a 1:1 mapping between Pool events and raw amounts, but liquidations have N:1 relationships (one Pool event, multiple amounts for different token operations).

This is similar to how flash loans have multiple operations from a single event - the extraction layer needs to be aware of the context (which scaled token event is being enriched) to return the correct amount.

**Refactoring:**

1. **Redesign RawAmountExtractor**: Change the interface to accept both the pool event AND the scaled token event being enriched, allowing contextual extraction.

2. **Add Event Type Context**: Pass the `ScaledTokenEventType` to the extractor so it can return the appropriate amount for liquidations.

3. **Update Enrichment Flow**: Modify `ScaledEventEnricher.enrich()` to pass context to the extractor.

4. **Test Coverage**: Add unit tests for liquidation events covering:
   - Debt burn with debtToCover
   - Collateral burn with liquidatedCollateralAmount
   - Multiple collateral transfers in single liquidation

**Example Fix Structure:**

```python
# In extraction.py
def extract(self, scaled_event_type: ScaledTokenEventType | None = None) -> int:
    """
    Extract raw amount from the Pool event.
    
    For LiquidationCall events, scaled_event_type determines which amount to return:
    - DEBT_BURN: returns debtToCover
    - COLLATERAL_BURN/TRANSFER: returns liquidatedCollateralAmount
    """
    extractor = self._get_extractor()
    return extractor(self.pool_event, scaled_event_type)

# In enrichment.py
calculator = ScaledAmountCalculator(...)
scaled_amount = calculator.calculate(
    event_type=scaled_event.event_type,  # Pass event type for context
    raw_amount=raw_amount,
    index=scaled_event.index,
)
```

**Verification:**

After the fix, the liquidation should process correctly:
```
_position_scaled_token_operation burn: delta=-21830, new_balance=0
```

**Related Issues:**
- Issue 0007: Interest Accrual Burn Amount Zeroed in Enrichment (similar event handling complexity)
- Issue 0008: INTEREST_ACCRUAL Debt Burn Missing Pool Event Reference (related to missing pool event context)

**Filename:** 0009_update_output.log
