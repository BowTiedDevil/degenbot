# 0048 - INTEREST_ACCRUAL Collateral Mint Amount vs Balance Increase Mismatch

**Issue:** 1 Wei Rounding Error in Collateral Balance During REPAY_WITH_ATOKENS with Interest Accrual

**Date:** 2026-03-21

## Symptom

```
AssertionError: Balance verification failure for AaveV3Asset(...).
User 0x33834d12f270b47e41b19de3294781554fC2e743 scaled balance (58095448259)
does not match contract balance (58095448258) at block 24057698

Difference: 1 wei
```

## Investigation Summary

The issue is a 1 wei discrepancy in the collateral position balance calculation during a REPAY_WITH_ATOKENS operation that includes an INTEREST_ACCRUAL event. The local code calculates a balance that is 1 wei higher than the actual contract balance.

## Transaction Details

- **Hash:** `0x6a602cd01bee839c57431644b313d815e437796cba307dcbc0f68575038a08a3`
- **Block:** 24057698
- **Type:** REPAY_WITH_ATOKENS with INTEREST_ACCRUAL
- **User:** `0x33834d12f270b47e41b19de3294781554fC2e743`
- **Asset:** USDC (aUSDC: 0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c)
- **Pool Revision:** 9
- **Token Revisions:** aToken=4, vToken=4

## On-Chain Verification

```bash
# Pre-transaction aUSDC scaled balance (block 24057697)
cast call 0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c "scaledBalanceOf(address)" \
  0x33834d12f270b47e41b19de3294781554fC2e743 --block 24057697
# Result: 0xd86c230f9 = 58,095,448,313

# Post-transaction aUSDC scaled balance (block 24057698)
cast call 0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c "scaledBalanceOf(address)" \
  0x33834d12f270b47e41b19de3294781554fC2e743 --block 24057698
# Result: 0xd86c230c2 = 58,095,448,258
```

**Actual contract behavior:**
- Initial balance: 58,095,448,313 wei
- Final balance: 58,095,448,258 wei
- Actual change: -55 wei

**Local code behavior:**
- Initial balance: 58,095,448,313 wei
- Final balance: 58,095,448,259 wei (1 wei higher than expected)
- Calculated change: -54 wei

## Event Analysis

Transaction events for user `0x33834d12f270b47e41b19de3294781554fC2e743`:

### Log Index 75: Transfer (vToken Burn for debt repayment)
- **Contract:** 0x72E95b8931767C79bA4EeE721354d6E99a61D004 (variableDebtEthUSDC)
- **From:** 0x33834d12f270b47e41b19de3294781554fC2e743
- **To:** 0x0000000000000000000000000000000000000000
- **Value:** 63

### Log Index 76: Burn (vToken)
- **Contract:** 0x72E95b8931767C79bA4EeE721354d6E99a61D004
- **From:** 0x33834d12f270b47e41b19de3294781554fC2e743
- **Amount:** 63
- **Balance Increase:** 0
- **Index:** 1209559452237207597924274322

### Log Index 77: ReserveDataUpdated (INTEREST_ACCRUAL)
- **Contract:** 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 (Pool)
- **Asset:** 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48 (USDC)
- **Liquidity Index:** 1156521799053308508395759823
- **Variable Borrow Index:** 1209559452237207597924274322

### Log Index 78: Transfer (aUSDC Interest Mint)
- **Contract:** 0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c (aUSDC)
- **From:** 0x0000000000000000000000000000000000000000
- **To:** 0x33834d12f270b47e41b19de3294781554fC2e743
- **Value:** 463,079,194

### Log Index 79: Mint (aUSDC)
- **Contract:** 0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c
- **Caller:** 0x33834d12f270b47e41b19de3294781554fC2e743
- **OnBehalfOf:** 0x33834d12f270b47e41b19de3294781554fC2e743
- **Amount:** 463,079,194
- **Balance Increase:** 463,079,257
- **Index:** 1156521799053308508395759823

### Log Index 80: Repay (Pool)
- **Contract:** 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2
- **Reserve:** 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48 (USDC)
- **User:** 0x33834d12f270b47e41b19de3294781554fC2e743
- **Repayer:** 0x33834d12f270b47e41b19de3294781554fC2e743
- **Amount:** 63
- **UseATokens:** true

## Critical Finding

**The Mint event at logIndex 79 represents the NET interest accrual after accounting for the collateral burn.**

In a REPAY_WITH_ATOKENS operation where interest exceeds the repayment:
1. Interest accrues: 463,079,257 (balance_increase)
2. Repayment burns: 63 aTokens
3. Net mint: 463,079,257 - 63 = 463,079,194 (amount field)

The Mint event's `amount` field is the NET interest after the burn, NOT the raw interest.

**Key Observation:**
- `balance_increase` (463,079,257) = gross interest accrued
- `amount` (463,079,194) = net interest after burn (balance_increase - repay_amount)
- Difference: 463,079,257 - 463,079,194 = 63 = repay_amount

## Root Cause Analysis

The 1 wei discrepancy arises because:

1. **Contract Behavior:**
   - Gross interest accrued: 463,079,257
   - Burn amount: 63 (repayment)
   - Net mint: 463,079,194
   - Scaled amount calculation: 463,079,194 * RAY / 1156521799053308508395759823

2. **Local Code Behavior:**
   - The INTEREST_ACCRUAL operation processes the Mint event with `scaled_amount=0` (informational only)
   - The enrichment layer extracts `raw_amount = scaled_event.amount` (463,079,194)
   - But the actual balance change should be based on the NET interest

3. **The Math:**
   ```
   Scaled amount = floor(net_interest * RAY / index)
                 = floor(463079194 * 10^27 / 1156521799053308508395759823)
                 = 400412514
   
   But actual burn = floor(repay_amount * RAY / index)
                   = floor(63 * 10^27 / 1156521799053308508395759823)
                   = 54
   ```

**THE ISSUE:** The local code is not correctly calculating the collateral adjustment when interest exceeds the burn amount in REPAY_WITH_ATOKENS operations.

## Fix

### Fix 1: Add Special Case in Event Matching

**File:** `src/degenbot/cli/aave_transaction_operations.py` (lines 1873-1876)

When matching collateral adjustment events for REPAY_WITH_ATOKENS, the calculation for COLLATERAL_MINT events (when interest > repayment) should use the burn amount, not the sum:

```python
# For COLLATERAL_MINT in REPAY_WITH_ATOKENS:
# - balance_increase = gross interest accrued
# - amount = net interest (balance_increase - burn_amount)
# - The actual burn amount = balance_increase - amount
if ev.event_type == ScaledTokenEventType.COLLATERAL_MINT:
    adjustment = ev.balance_increase - ev.amount  # This equals the burn amount
else:
    # For burns, total adjustment = principal + interest
    adjustment = ev.amount + ev.balance_increase
```

### Fix 2: Add Enrichment Special Case

**File:** `src/degenbot/aave/enrichment.py` (after line 266)

Added special case for REPAY_WITH_ATOKENS + COLLATERAL_MINT when interest exceeds repayment:

```python
elif (
    # Special case: When interest exceeds repayment amount in
    # REPAY_WITH_ATOKENS, the aToken contract emits a Mint event with
    # amount = balance_increase - repay_amount. Use COLLATERAL_BURN
    # calculation (ceil rounding) to match contract behavior.
    operation.operation_type == OperationType.REPAY_WITH_ATOKENS
    and scaled_event.event_type == ScaledTokenEventType.COLLATERAL_MINT
    and scaled_event.balance_increase is not None
    and scaled_event.amount < scaled_event.balance_increase
):
    # Use COLLATERAL_BURN for burn rounding (ceil)
    calculation_event_type = ScaledTokenEventType.COLLATERAL_BURN
```

### Fix 3: Route to Burn Processing

**File:** `src/degenbot/cli/aave.py` (after line 2827)

Added routing logic to treat COLLATERAL_MINT as burn when interest exceeds repayment:

```python
elif (
    # Special case: In REPAY_WITH_ATOKENS, when interest exceeds repayment,
    # the Mint event's amount field represents net interest
    # (balance_increase - repay_amount). Treat as burn.
    operation.operation_type == OperationType.REPAY_WITH_ATOKENS
    and scaled_event.balance_increase is not None
    and scaled_event.amount < scaled_event.balance_increase
):
    logger.debug(
        f"REPAY_WITH_ATOKENS: Treating COLLATERAL_MINT as burn - "
        f"interest exceeds repayment"
    )
    _process_collateral_burn_with_match(...)
```

## Key Insight

> **In REPAY_WITH_ATOKENS operations where interest exceeds the repayment, the Mint event's `amount` field represents the NET interest (gross interest - burn amount), not the gross interest.**

The Aave V3 aToken contract emits Mint events differently when the user has accrued interest that exceeds the burn amount:
- `balance_increase` = gross interest accrued
- `amount` = net interest after accounting for the burn (balance_increase - burn_amount)

The key architectural decision was to **route COLLATERAL_MINT events to burn processing** rather than trying to process them as mints with negative amounts. This preserves the invariant that mint events add to balances and burn events subtract from balances.

## Files to Investigate

1. `src/degenbot/aave/enrichment.py`
   - Lines 95-102: INTEREST_ACCRUAL handling
   - Review how scaled_amount is calculated for collateral interest

2. `src/degenbot/cli/aave_transaction_operations.py`
   - Lines 1823-1888: `_find_collateral_adjustment_event()`
   - Review how collateral adjustments are calculated for REPAY_WITH_ATOKENS

3. `src/degenbot/cli/aave.py`
   - Lines 3051-3123: `_process_collateral_mint_with_match()`
   - Review how collateral mint events are processed

## Testing Considerations

Test cases needed:
1. REPAY_WITH_ATOKENS where interest > repayment (net mint)
2. REPAY_WITH_ATOKENS where repayment > interest (net burn)
3. Standard REPAY operations to ensure no regression
4. Other INTEREST_ACCRUAL operations to ensure no regression

## References

- `_find_collateral_adjustment_event()` in aave_transaction_operations.py:1823-1888
- `ScaledEventEnricher.enrich()` in enrichment.py:64-300
- Aave V3 aToken contract: `contract_reference/aave/AToken/rev_4.sol`
- Related issues: 0040, 0031, 0016 (REPAY rounding errors)

---

**Status:** ✅ FIXED

## Fix Summary

The fix involved three coordinated changes:

1. **Event Matching** (`aave_transaction_operations.py:1873-1876`): Corrected the adjustment calculation for COLLATERAL_MINT events in REPAY_WITH_ATOKENS - when interest exceeds repayment, use `balance_increase - amount` (the burn amount) instead of `amount + balance_increase`.

2. **Enrichment Layer** (`enrichment.py:267-279`): Added special case to use COLLATERAL_BURN calculation (ceil rounding) for REPAY_WITH_ATOKENS + COLLATERAL_MINT when `amount < balance_increase`.

3. **Processing Layer** (`aave.py:2829-2847`): Added routing logic to treat COLLATERAL_MINT as a burn event (route to `_process_collateral_burn_with_match`) when interest exceeds repayment.

## Verification

After fix:
- Transaction `0x6a602cd...` processes correctly
- USDC collateral balance for user `0x33834d12f270b47e41b19de3294781554fC2e743`: 58,095,448,258 wei (matches contract)
- Aave market update completes successfully: "AaveV3Market successfully updated to block 24,057,698"

## Refactoring

1. **Unified special case pattern**: The fix follows the existing pattern used for WITHDRAW + COLLATERAL_MINT (interest > withdrawal), ensuring consistency in handling "interest exceeds operation amount" scenarios.

2. **Routing over calculation**: Rather than trying to calculate negative amounts or adjust the scaled_amount, the fix routes to the appropriate processor (burn vs mint). This preserves the invariant that mint processors always add and burn processors always subtract.

3. **Clear separation of concerns**: Event matching calculates the correct adjustment amount, enrichment determines the calculation type, and processing handles the balance change.
