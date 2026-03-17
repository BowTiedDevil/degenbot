# Issue: MINT_TO_TREASURY Validation Error on Pool Revision 8

## Date
2026-03-16

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...). 
User AaveV3User(...) scaled balance (131851388478) 
does not match contract balance (131851390577) at block 23001515

EnrichmentError: scaled_amount cannot be None for validation
```

## Root Cause

The MINT_TO_TREASURY validation skip in `src/degenbot/aave/models.py` only applied to pool revision >= 9, but block 23002818 has pool revision 8. When enrichment sets `scaled_amount = None` for MINT_TO_TREASURY operations (because the calculation requires position data), validation fails with "scaled_amount cannot be None for validation".

### Why This Happens

1. The enrichment layer (`src/degenbot/aave/enrichment.py:102`) sets `scaled_amount = None` for MINT_TO_TREASURY operations because the correct calculation requires:
   - User's current scaled balance
   - User's last index
   - These are only available during processing in `aave.py`

2. The validation in `src/degenbot/aave/models.py:210` only skipped validation for pool revision >= 9:
   ```python
   if event_type == ScaledTokenEventType.COLLATERAL_MINT and pool_rev >= 9:
       return self
   ```

3. Block 23002818 has pool revision 8, so validation wasn't skipped and failed when `scaled_amount is None`.

## Transaction Details

| Field | Value |
|-------|-------|
| **Transaction Hash** | 0x78a4a6f0a4f385b2991a0ec69dc5ba0a47c187287560a685eb66e5bdd43b8117 |
| **Block** | 23002818 |
| **Type** | MINT_TO_TREASURY |
| **Pool Revision** | 8 |
| **aToken Revision** | 4 |
| **Treasury** | 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c |
| **Asset** | Multiple (WETH, USDC, etc.) |

## Fix

**File**: `src/degenbot/aave/models.py`

**Location**: Lines 204-212 (`validate_scaled_amount` method in `IndexScaledEvent`)

**Change**: Replace the pool revision check with a check for `scaled is None`:

```python
# OLD (only pool revision >= 9):
if event_type == ScaledTokenEventType.COLLATERAL_MINT and pool_rev >= 9:
    # Accept the scaled amount as-is (may be None during enrichment)
    return self

# NEW (all revisions when scaled_amount is None):
if event_type == ScaledTokenEventType.COLLATERAL_MINT and scaled is None:
    # Accept the scaled amount as-is (None during enrichment, calculated later)
    return self
```

**Rationale**:
1. More robust - applies to all pool revisions where MINT_TO_TREASURY sets `scaled_amount = None`
2. Doesn't break cases where MINT_TO_TREASURY has a calculated `scaled_amount`
3. Aligns with the comment which says "Accept the scaled amount as-is (may be None during enrichment)"
4. The actual calculation happens later in `aave.py` with position context

## Verification

**Test Results**:
- Block 23002818: ✅ Passed (original failing block)
- Blocks 23002818-23002868: ✅ 50 blocks passed

## Key Insight

The previous fix (Issue 0014/0015) assumed MINT_TO_TREASURY only needed special handling for pool revision >= 9, but the pattern of setting `scaled_amount = None` during enrichment and calculating later applies to ALL pool revisions. The validation should skip based on whether `scaled_amount` is None, not based on pool revision.

## Related Issues

- Issue 0014: MINT_TO_TREASURY AccruedToTreasury Calculation Error
- Issue 0015: MINT_TO_TREASURY BalanceTransfer Amount Not Used

## References

- Contract: `contract_reference/aave/Pool/rev_8.sol` (mintToTreasury)
- Files:
  - `src/degenbot/aave/models.py` (validation logic)
  - `src/degenbot/aave/enrichment.py` (enrichment layer)
  - `src/degenbot/cli/aave.py` (processing with position context)
