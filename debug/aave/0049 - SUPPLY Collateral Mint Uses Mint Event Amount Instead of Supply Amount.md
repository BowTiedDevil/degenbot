# 0049 - SUPPLY Collateral Mint Uses Mint Event Amount Instead of Supply Amount

**Issue:** 1 Wei Rounding Error in SUPPLY Collateral Balance Verification

**Date:** 2026-03-21

## Symptom

```
AssertionError: Balance verification failure for AaveV3Asset(...).
User 0x246E20bF778b3e16cB71eca535f40f8C4E6c4185 scaled balance (9452400556120464)
does not match contract balance (9452400556120465) at block 24247936

Difference: 1 wei
```

## Investigation Summary

The issue is a 1 wei discrepancy in the collateral position balance calculation during a SUPPLY operation. The local code calculates a balance that is 1 wei lower than the actual contract balance.

## Transaction Details

- **Hash:** `0x55a9a6541dca588f54989edf9747db92f30566e1f9863cfc95a16960a6f97c24`
- **Block:** 24247936
- **Type:** SUPPLY
- **User:** `0x246E20bF778b3e16cB71eca535f40f8C4E6c4185`
- **Asset:** WETH (aWETH: 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8)
- **Pool Revision:** 10
- **Token Revisions:** aToken=5, vToken=5

## On-Chain Verification

```bash
# Pre-transaction aWETH scaled balance (block 24247935)
cast call 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8 "scaledBalanceOf(address)" \
  0x246E20bF778b3e16cB71eca535f40f8C4E6c4185 --block 24247935
# Result: 0

# Post-transaction aWETH scaled balance (block 24247936)
cast call 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8 "scaledBalanceOf(address)" \
  0x246E20bF778b3e16cB71eca535f40f8C4E6c4185 --block 24247936
# Result: 0x219B37B2F5 = 9,452,400,556,120,465
```

**Actual contract behavior:**
- Initial balance: 0 wei
- Final balance: 9,452,400,556,120,465 wei
- Actual change: +9,452,400,556,120,465 wei

**Local code behavior:**
- Initial balance: 0 wei
- Final balance: 9,452,400,556,120,464 wei (1 wei lower than expected)
- Calculated change: +9,452,400,556,120,464 wei

## Event Analysis

Transaction events for user `0x246E20bF778b3e16cB71eca535f40f8C4E6c4185`:

### Log Index 201: Deposit (WETH)
- **Contract:** 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2 (WETH)
- **Dst:** 0xd01607c3C5eCABa394D8be377a08590149325722 (WETH Gateway)
- **Wad:** 10,000,000,000,000,000

### Log Index 202: ReserveDataUpdated (Pool)
- **Contract:** 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 (Pool)
- **Reserve:** 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2 (WETH)
- **Liquidity Index:** 1057932314720302672289370287

### Log Index 203: Transfer (WETH to aWETH)
- **Contract:** 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2 (WETH)
- **From:** 0xd01607c3C5eCABa394D8be377a08590149325722 (Gateway)
- **To:** 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8 (aWETH)
- **Value:** 10,000,000,000,000,000

### Log Index 204: Transfer (aWETH Mint)
- **Contract:** 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8 (aWETH)
- **From:** 0x0000000000000000000000000000000000000000
- **To:** 0x246E20bF778b3e16cB71eca535f40f8C4E6c4185 (User)
- **Value:** 9,999,999,999,999,999

### Log Index 205: Mint (aWETH)
- **Contract:** 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8 (aWETH)
- **Caller:** 0xd01607c3C5eCABa394D8be377a08590149325722 (Gateway)
- **OnBehalfOf:** 0x246E20bF778b3e16cB71eca535f40f8C4E6c4185 (User)
- **Amount:** 9,999,999,999,999,999
- **Balance Increase:** 0
- **Index:** 1057932314720302672289370287

### Log Index 206: ReserveUsedAsCollateralEnabled (Pool)
- **Contract:** 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 (Pool)
- **Reserve:** 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2 (WETH)
- **User:** 0x246E20bF778b3e16cB71eca535f40f8C4E6c4185 (User)

### Log Index 207: Supply (Pool)
- **Contract:** 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 (Pool)
- **Reserve:** 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2 (WETH)
- **User:** 0xd01607c3C5eCABa394D8be377a08590149325722 (Gateway)
- **OnBehalfOf:** 0x246E20bF778b3e16cB71eca535f40f8C4E6c4185 (User)
- **Amount:** 10,000,000,000,000,000
- **ReferralCode:** 0

## Critical Finding

**The Mint event's `amount` field is the scaled-down representation of the actual minted tokens, not the original supply amount.**

When a user supplies assets to Aave V3:

1. **Pool receives supply amount:** 10,000,000,000,000,000 wei
2. **Pool calculates scaled amount:** 
   ```
   scaledAmount = floor(supply_amount * RAY / index)
                = floor(10000000000000000 * 10^27 / 1057932314720302672289370287)
                = 9452400556120465
   ```
3. **Pool calls aToken.mint(scaledAmount):** 9452400556120465
4. **aToken calculates display amount for Mint event:**
   ```
   displayAmount = floor(scaledAmount * index / RAY)
                 = floor(9452400556120465 * 1057932314720302672289370287 / 10^27)
                 = 9999999999999999
   ```

**Key Observation:**
- `Supply` event amount: 10,000,000,000,000,000 (original supply)
- `Mint` event amount: 9,999,999,999,999,999 (display amount after two rounding operations)
- Difference: 1 wei

## Root Cause Analysis

The 1 wei discrepancy arises because:

1. **Contract Behavior:**
   - Supply amount: 10,000,000,000,000,000
   - Scaled amount calculation: floor(10,000,000,000,000,000 * RAY / index) = 9452400556120465
   - Actual scaled balance increase: 9452400556120465

2. **Local Code Behavior:**
   - The enrichment layer extracts `raw_amount` from the Pool's Supply event
   - For Pool rev 10, it calculates `scaled_amount = floor(raw_amount * RAY / index)`
   - **BUT** the processing layer may be using the Mint event's `amount` field (9,999,999,999,999,999) to calculate the scaled amount

3. **The Math:**
   ```
   Using Supply amount:
   Scaled = floor(10000000000000000 * 10^27 / 1057932314720302672289370287)
          = 9452400556120465 ✓ (matches contract)
   
   Using Mint event amount:
   Scaled = floor(9999999999999999 * 10^27 / 1057932314720302672289370287)
          = 9452400556120464 ✗ (1 wei less)
   ```

**THE ISSUE:** The local code is calculating the scaled balance from the Mint event's `amount` field instead of the Supply event's `amount` field for SUPPLY operations.

Looking at the enrichment code in `enrichment.py`:
- Line 121-240: For operations with pool events, it extracts `raw_amount` from the pool event and calculates `scaled_amount` using TokenMath
- For SUPPLY operations, the `RawAmountExtractor` extracts the supply amount correctly

However, the issue may be in the processing layer where the CollateralMintEvent is created. The `scaled_amount` should come from the enrichment layer (which correctly calculates from the supply amount), but the processing layer might be recalculating it from the Mint event's `value` field.

## Fix

**File:** `src/degenbot/cli/aave.py` (line 2333-2337)

The cleanest fix follows the existing pattern used for `DebtMintEvent` and `CollateralBurnEvent` - pass the pre-calculated `scaled_amount` as the `scaled_delta` parameter to `process_mint_event()`:

```python
mint_result: ScaledTokenMintResult = collateral_processor.process_mint_event(
    event_data=event,
    previous_balance=position.balance,
    previous_index=position.last_index or 0,
    scaled_delta=event.scaled_amount,  # <-- ADD THIS LINE
)
```

This fix:
1. **Follows existing patterns** - Same pattern used for `DebtMintEvent` (line 2376) and `CollateralBurnEvent` (line 2355)
2. **Minimal change** - Single line addition
3. **Preserves architecture** - Uses `enriched_event.scaled_amount` (calculated from Pool event) instead of recalculating from Mint event
4. **No special cases** - Leverages existing processor logic that checks `if scaled_delta is not None` (see `collateral/v5.py:146`)

### Alternative Fixes Considered

**Alternative 1: Modify the processor to always use `event.scaled_amount`**
- Would require changing processor logic
- Less flexible for edge cases (e.g., interest accrual where scaled_amount=0)
- More invasive change

**Alternative 2: Add tolerance in verification layer**
- Would mask real errors
- Doesn't fix the root cause
- Makes verification less strict

**Alternative 3: Recalculate in `_process_collateral_mint_with_match`**
- Duplicates enrichment logic
- Violates single source of truth principle
- More complex and error-prone

The chosen fix is the **cleanest architectural solution** because it:
- Maintains the separation between enrichment (calculates amounts) and processing (applies amounts)
- Uses the existing `scaled_delta` parameter designed for this purpose
- Follows the established pattern for other event types

## Key Insight

> **In SUPPLY operations, the Mint event's `amount` field represents the display amount after two successive rounding operations (supply→scaled→display), which can be 1 wei less than the original supply amount. The enrichment layer correctly calculates the scaled amount from the Pool's Supply event, but the processing layer must use this pre-calculated value instead of recalculating from the Mint event.**

This is similar to the issue fixed in debug report 0048 (REPAY_WITH_ATOKENS), where using the wrong source amount caused rounding errors. The solution is the same: use the Pool event's amount as the source of truth for calculating scaled amounts.

## Files to Investigate

1. `src/degenbot/aave/enrichment.py`
   - Lines 121-240: Amount extraction and scaled calculation
   - Verify that SUPPLY operations correctly extract and calculate scaled amounts

2. `src/degenbot/cli/aave.py`
   - Lines 3071-3142: `_process_collateral_mint_with_match()`
   - Review how CollateralMintEvent is created and whether it uses enriched_event.scaled_amount

3. `src/degenbot/cli/aave_transaction_operations.py`
   - Lines related to SUPPLY operation creation
   - Verify that scaled_amount is correctly propagated from enrichment to processing

## Testing Considerations

Test cases needed:
1. SUPPLY operations with various amounts and liquidity indices
2. SUPPLY where supply_amount * RAY / index has a fractional part near 0.5
3. Verify no regression in other operations (WITHDRAW, REPAY, etc.)

## References

- `_process_collateral_mint_with_match()` in aave.py:3071-3142
- `ScaledEventEnricher.enrich()` in enrichment.py:64-370
- Aave V3 Pool contract rev_10: `getATokenMintScaledAmount()` uses floor rounding
- Related issues: 0048 (similar pattern in REPAY_WITH_ATOKENS)

---

**Status:** ✅ FIXED AND VERIFIED

## Proposed Fix Summary

The issue is that the processing layer is using the Mint event's `amount` field to calculate the scaled balance, but this field is already a rounded-down display amount. The correct approach is:

1. **Enrichment layer:** Calculate `scaled_amount` from the Pool's Supply event amount using floor rounding
2. **Processing layer:** Use the enriched `scaled_amount` directly without recalculation

This ensures the local calculated balance matches the contract's scaled balance exactly.

## Verification

After implementing the fix:

```bash
$ uv run degenbot aave update
...
[Pool rev 10] Processing 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2 collateral mint at block 24247936
Processing scaled token operation (CollateralMintEvent) for aToken revision 5
...
AaveV3Market successfully updated to block 24,247,936
```

✅ **Verification passed** - No balance verification errors
✅ **Block 24247936 processed successfully** - Transaction `0x55a9a6541dca588f54989edf9747db92f30566e1f9863cfc95a16960a6f97c24` processed correctly
✅ **User 0x246E20bF778b3e16cB71eca535f40f8C4E6c4185** - WETH collateral balance now matches contract exactly

## Refactoring

1. **Unified source of truth:** The Pool event's amount should always be the source for calculating scaled amounts, not the token events which may have rounding artifacts.

2. **Explicit propagation:** Make it clear in the code that `enriched_event.scaled_amount` is the authoritative value and should not be recalculated. The fix follows this by passing `scaled_delta=event.scaled_amount` to the processor.

3. **Documentation:** Add comments explaining that Mint event amounts are display amounts (already rounded) and should not be used for scaled balance calculations.

4. **Consistency across event types:** The fix aligns `CollateralMintEvent` processing with the patterns already established for `DebtMintEvent` and `CollateralBurnEvent`, improving code uniformity.

## Architecture Decision

The architecture already supports the correct fix - the enrichment layer calculates scaled amounts correctly from Pool events. The issue was ensuring the processing layer uses these pre-calculated values rather than deriving amounts from Mint event fields.

**Flow before fix:**
```
Pool Event (amount) → Enrichment (calculates scaled_amount) → Processing (ignores scaled_amount)
                                                                    ↓
Mint Event (amount) → Processor.recalculate() → Wrong balance (1 wei off)
```

**Flow after fix:**
```
Pool Event (amount) → Enrichment (calculates scaled_amount) → Processing (uses scaled_amount via scaled_delta parameter)
                                                                    ↓
Mint Event (amount) → Processor (for reference only) → Correct balance
```

The fix is architecturally sound because it:
- Leverages the existing `scaled_delta` parameter designed for pre-calculated amounts
- Maintains the separation between enrichment (calculation) and processing (application)
- Follows established patterns for other event types
- Makes the code more consistent and maintainable
