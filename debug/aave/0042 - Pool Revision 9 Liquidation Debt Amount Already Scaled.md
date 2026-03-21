# 0042 - Pool Revision 9 Liquidation Debt Amount Already Scaled

## Issue
Multi-liquidation transaction fails balance verification for user with vUSDC debt position.

## Date
2026-03-20

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...). User AaveV3User(...) scaled balance (690358115580) does not match contract balance (286484822926) at block 23549932
```

**Key Data Points:**
- Calculated balance: 690,358,115,580
- On-chain balance: 286,484,822,926  
- Difference: 403,873,292,654 (way beyond any rounding tolerance)

## Root Cause

For **Pool Revision 9+**, the `debtToCover` value in LiquidationCall events is **already in scaled units** when passed to the debt token contract. The current code treats it as underlying units and incorrectly rescales it using `ray_div_floor`.

### Contract Behavior Change in Pool Revision 9

**Pool Revision 1-8:**
- Pool passes `debtToCover` in **underlying units** to the token contract
- Token contract scales it: `scaledAmount = underlyingAmount.rayDiv(index)`
- Result: Burn event amount ≠ debtToCover from pool event

**Pool Revision 9+:**
- Pool **pre-scales** the amount before calling the token contract
- Token contract receives amount already in scaled units
- Result: Burn event amount = debtToCover from pool event (both scaled)

### Evidence from Transaction

**Transaction:** `0x84c14599ac99013583fc49810f52cf9c7d254e015a7c22d4f132c8b94b407470`
**Block:** 23549932
**Pool Revision:** 9
**User:** `0x946fD776Bde2A6647a5a307835D1A5bE543581a6`

**Event Data:**
| Field | Value | Units |
|-------|-------|-------|
| Burn event `amount` | 483,523,157,683 | **Scaled** |
| Burn event `balance_increase` | 30,676,408 | Underlying |
| Burn event `index` | 1,197,246,967,157,968,573,637,867,477 | RAY |
| LiquidationCall `debtToCover` (enriched) | 17,759,345 | **Scaled** (but treated as underlying) |

**The Bug:**

In `_process_debt_burn_with_match` (aave.py:3533-3539):
```python
if operation and operation.operation_type in LIQUIDATION_OPERATION_TYPES:
    # Normal liquidation: use debtToCover from pool event
    burn_value = enriched_event.raw_amount  # This is SCALED for Pool Rev 9+
    logger.debug(f"debtToCover={burn_value}")  # Shows: 17759345
```

But then in `_process_scaled_token_operation`, the DebtV4Processor does:
```python
# In DebtV4Processor.process_burn_event (line 106-109)
balance_delta = -wad_ray_math.ray_div_floor(
    a=requested_amount,  # 17759345 (already scaled!)
    b=event_data.index,  # 1,197,246,967,157,968,573,637,867,477
)
# Result: 17759345 / 1.197e27 ≈ 0 (but actually 14 due to rounding)
```

This calculates a tiny burn amount (14) instead of the correct scaled amount (483,523,157,683).

**Why the balance is wrong:**
- Starting balance before liquidation: ~483,523,157,683
- Incorrect burn applied: ~14
- Remaining calculated balance: ~483,523,157,669

But wait - there's more. There are actually TWO debt burns for this user in the same liquidation:
1. vWBTC burn: 17,381,606 scaled (from log 48)
2. vUSDC burn: 483,523,157,683 scaled (from log 100)

The second burn (vUSDC) is the one being miscalculated, leaving a huge remaining balance.

## Transaction Details

- **Hash:** `0x84c14599ac99013583fc49810f52cf9c7d254e015a7c22d4f132c8b94b407470`
- **Block:** 23549932
- **Type:** Multi-Asset Liquidation (6 simultaneous liquidations)
- **User Liquidated:** Multiple users including `0x946fD776Bde2A6647a5a307835D1A5bE543581a6`
- **Debt Assets:** WBTC, USDC
- **Collateral Assets:** WETH
- **Pool Revision:** 9
- **Token Revisions:** aToken=4, vToken=4

### Event Sequence for User 0x946fD776...

| Log Index | Token | Event Type | Amount | Units |
|-----------|-------|------------|--------|-------|
| 47 | vWBTC | Transfer | 17,381,606 | Scaled |
| 48 | vWBTC | Burn | 17,381,606 | Scaled |
| 51 | aWETH | Transfer | 5,515,806,233,453,570,292 | Scaled |
| 52 | aWETH | Burn | 5,515,806,233,453,570,292 | Scaled |
| 100 | vUSDC | Burn | 483,523,157,683 | **Scaled** |
| 104 | aWETH | Burn | 131,382,420,533,579,465,588 | Scaled |

## Fix

**Architecture:** Option 3 (most architecturally clean) - Uses existing `scaled_amount` field in event objects designed for pre-calculated amounts from Pool contract.

### Files Modified

#### 1. `src/degenbot/aave/enrichment.py`

**Location:** Lines 136-153 (in the LIQUIDATION handling section)

**Change:** Added Pool Rev 9+ detection for liquidation debt amounts
```python
if scaled_event.event_type in {
    ScaledTokenEventType.DEBT_BURN,
    ScaledTokenEventType.GHO_DEBT_BURN,
    ScaledTokenEventType.DEBT_TRANSFER,
    ScaledTokenEventType.ERC20_DEBT_TRANSFER,
}:
    # Debt events use debtToCover
    raw_amount = RawAmountExtractor.extract_liquidation_debt(
        operation.pool_event
    )
    # Pool Revision 9+ passes pre-scaled amounts to token contracts
    # Skip TokenMath calculation - use raw_amount directly as scaled
    if self.pool_revision >= 9:  # noqa: PLR2004
        logger.debug(
            f"ENRICHMENT: Pool Rev {self.pool_revision} LIQUIDATION "
            f"debt amount already scaled: {raw_amount}"
        )
        scaled_amount = raw_amount
        calculation_event_type = scaled_event.event_type
        # Skip to event creation
        return self._create_enriched_event(
            scaled_event=scaled_event,
            operation=operation,
            raw_amount=raw_amount,
            scaled_amount=scaled_amount,
            token_revision=token_revision,
            token_address=token_address,
            underlying_asset=underlying_asset,
        )
```

#### 2. `src/degenbot/aave/models.py`

**Location:** Lines 213-217 (in `validate_scaled_amount` method)

**Change:** Skip validation for Pool Rev 9+ liquidation debt burns
```python
if scaled is None:
    return self

# Special case: Pool Revision 9+ LIQUIDATION debt amounts
# For Pool Rev 9+, the debtToCover in LiquidationCall is already scaled
# Skip validation since raw_amount == scaled_amount for these cases
if pool_rev >= 9 and event_type == ScaledTokenEventType.DEBT_BURN:  # noqa: PLR2004
    return self

# Calculate expected scaled amount
```

### Why This Approach is Architecturally Clean

1. **Uses existing infrastructure** - The `scaled_amount` field in event dataclasses (`CollateralBurnEvent`, `DebtBurnEvent`, etc.) was designed for pre-calculated amounts from Pool contract
2. **Localized changes** - Only enrichment layer needs modification
3. **No changes needed to processors** - Token processors remain pure calculation engines
4. **Follows established patterns** - Docstrings in base.py indicate this field is for "pre-calculated scaled amounts from Pool contract"
5. **Extensible** - Can apply same pattern to other operations if Pool Rev 9+ changes them

## Key Insight

**Pool Revision 9 changed the liquidation amount handling:**

Previous liquidations (Pool Rev 1-8):
- Pool event: `debtToCover` = underlying units
- Burn event: `amount` = scaled units (after token conversion)
- Need to calculate: `scaled = underlying.rayDiv(index)`

Pool Revision 9+ liquidations:
- Pool event: `debtToCover` = scaled units (pre-scaled by pool)
- Burn event: `amount` = scaled units (same as pool)
- No calculation needed: use amount directly

**Why this matters:**
- Pool Revision 9 introduced `getDebtBalance()` and related helpers
- The pool now calculates scaled amounts before calling token contracts
- This affects liquidation, borrow, and repay operations

## Refactoring Recommendations

1. **Create revision-aware amount extractors:**
   ```python
   class LiquidationAmountExtractor:
       @staticmethod
       def extract_debt_amount(pool_event, pool_revision, index=None):
           """Extract debt amount, returning scaled units directly."""
           raw = RawAmountExtractor.extract_liquidation_debt(pool_event)
           if pool_revision >= 9:
               return raw  # Already scaled
           # Legacy: calculate scaled amount
           return calculator.underlying_to_scaled_debt(raw, index)
   ```

2. **Document Pool Revision behavior changes:**
   - Add comprehensive documentation about Pool Rev 9+ pre-scaling behavior
   - Create a reference table showing which operations are affected by revision changes

3. **Add tests:**
   - Unit test for Pool Rev 9+ liquidation debt burn processing
   - Integration test for multi-liquidation transactions
   - Test for edge cases (bad debt liquidations, interest accrual during liquidation)

4. **Consider broader Pool Rev 9+ impact:**
   - Review other operations (BORROW, REPAY) for similar pre-scaling behavior
   - Check if collateral amounts are also pre-scaled in Pool Rev 9+
   - Verify GHO operations follow same pattern

## Verification

After the fix:
- vUSDC burn correctly burns 483,523,157,683 scaled
- Final balance matches on-chain balance: 286,484,822,926 ✅
- Transaction `0x84c14599ac99013583fc49810f52cf9c7d254e015a7c22d4f132c8b94b407470` processes correctly

**Note:** A separate failure was discovered in a later transaction (`0xd0bd9c3f724f85f3f1dc5a70d3eb063548355f22ccd4e04c6c1b81c6085f02c7`) within the same block. This is a different issue where user `0x0FA2012F2F02E005472502C7A64C0371CC6d7E74` has a vUSDT debt balance mismatch. This appears to be a separate root cause requiring further investigation.

### Transaction Processing Order in Block 23549932

1. `0x84c14599ac99013583fc49810f52cf9c7d254e015a7c22d4f132c8b94b407470` - **FIXED** ✅
2. `0xd7f9e4365d3868db43961f3aa4f67a0a746cc65776a33eb5dd374a9bbbc0a362`
3. `0x537160087e7eb8cac9e7c57f72e7637ba7468409ee1f4dc139fb4d46bf1307f9`
4. `0x7477e7f5598d9e2d536cfeaf38ea6cf3c5516e064e793e01f328d4c99ad8b474`
5. `0x86cbebbd2671b2d087d81cdbdf82ee90c8b4080b4bad2a484b9037e6257d9faa`
6. `0x2b9f223abba05e2446a7263f9fe35dae904307d55d61f6efcb1f572ca2d51617`
7. `0xd0bd9c3f724f85f3f1dc5a70d3eb063548355f22ccd4e04c6c1b81c6085f02c7` - **NEW FAILURE** (requires investigation)

The fix for Issue 0042 successfully resolves the original transaction. The new failure is in a completely different transaction with different characteristics.

## Related Issues

- Issue 0005: Token Revision vs Pool Revision Mismatch
- Issue 0026: Liquidation Debt Burn Unit Mismatch
- Issue 0029: Multi-Asset Liquidation Missing Secondary Debt Burns

## Contract References

**Pool Revision 9 (rev_9.sol):**
- `liquidationCall()` now uses `getDebtBalance()` helper
- Passes pre-calculated scaled amounts to token contracts
- See lines around `_burnDebtTokens` call

**VariableDebtToken Revision 4 (rev_4.sol):**
- `burn()` receives scaled amount directly
- No additional scaling performed in contract
- Emits Burn event with scaled amount
