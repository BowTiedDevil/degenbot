# 0040 - 1 Wei Rounding Error in REPAY_WITH_ATOKENS Debt Burn

**Issue:** Debt burn scaled amount calculation has 1 wei rounding error in REPAY_WITH_ATOKENS operations

**Date:** 2026-03-20

## Symptom

```
AssertionError: Balance verification failure for AaveV3Asset(...).
User 0xc64d755e5FAF51A39F5CEE1e492dD9268b5dB203 scaled balance (44677043213)
does not match contract balance (44677043212) at block 23245734

Difference: 1 wei
```

## Investigation Summary

The issue is a 1 wei discrepancy in the debt position balance calculation during a REPAY_WITH_ATOKENS operation. The local code burns 1 wei less than the contract, resulting in a balance that's 1 wei higher than expected.

## Transaction Details

- **Hash:** `0x4ea08cf2c8ddd47d18586e1cfedc7bd99cc64c534ef8056785d6bf4d747d6a83`
- **Block:** 23245734
- **Type:** REPAY_WITH_ATOKENS
- **User:** `0xc64d755e5FAF51A39F5CEE1e492dD9268b5dB203`
- **Asset:** EURC (aEthEURC: 0xAA6e91C82942aeAE040303Bf96c15a6dBcB82CA0, variableDebtEthEURC: 0x6c82c66622Eb360FC973D3F492f9D8E9eA538b08)
- **Pool Revision:** 9
- **Token Revisions:** aToken=4, vToken=4

## On-Chain Verification

```bash
# Pre-transaction vToken scaled balance (block 23245733)
cast call 0x6c82c66622eb360fc973d3f492f9d8e9ea538b08 "scaledBalanceOf(address)" \
  0xc64d755e5FAF51A39F5CEE1e492dD9268b5dB203 --block 23245733
# Result: 0x0000000000000000000000000000000000000000000000000000000a6a344bee = 44,731,485,166

# Post-transaction vToken scaled balance (block 23245734)
cast call 0x6c82c66622eb360fc973d3f492f9d8e9ea538b08 "scaledBalanceOf(address)" \
  0xc64d755e5FAF51A39F5CEE1e492dD9268b5dB203 --block 23245734
# Result: 0x0000000000000000000000000000000000000000000000000000000a66f5940c = 44,677,043,212
```

**Actual contract behavior:**
- Initial balance: 44,731,485,166 wei
- Final balance: 44,677,043,212 wei
- Actual burn: 54,441,954 wei

**Local code behavior:**
- Initial balance: 44,731,485,166 wei
- Final balance: 44,677,043,213 wei (1 wei higher than expected)
- Calculated burn: 54,441,953 wei (1 wei less than actual)

## Event Analysis

Transaction events for user `0xc64d755e5FAF51A39F5CEE1e492dD9268b5dB203`:

### Log Index 858 (0x35a) - Transfer (vToken Mint for interest accrual)
- **Contract:** 0x6c82c66622eb360fc973d3f492f9d8e9ea538b08 (variableDebtEthEURC)
- **Event:** Transfer
- **From:** 0x0000000000000000000000000000000000000000
- **To:** 0xc64d755e5FAF51A39F5CEE1e492dD9268b5dB203
- **Value:** 64,528,873

### Log Index 859 (0x35b) - Mint (vToken - interest accrual)
- **Contract:** 0x6c82c66622eb360fc973d3f492f9d8e9ea538b08
- **Event:** Mint
- **Caller:** 0xc64d755e5FAF51A39F5CEE1e492dD9268b5dB203
- **OnBehalfOf:** 0xc64d755e5FAF51A39F5CEE1e492dD9268b5dB203
- **Value:** 64,528,873
- **Balance Increase:** 119,446,532
- **Index:** 1008737856020467344683377256

### Log Index 860 (0x35c) - ReserveDataUpdated
- **New variableBorrowIndex:** 1008737856020467344683377256

### Log Index 861 (0x35d) - Transfer (aToken Burn)
- **Contract:** 0xaa6e91c82942aeae040303bf96c15a6dbcb82ca0 (aEthEURC)
- **From:** user
- **To:** 0x0000000000000000000000000000000000000000
- **Value:** 54,917,637

### Log Index 862 (0x35e) - Burn (aToken)
- **Contract:** 0xaa6e91c82942aeae040303bf96c15a6dbcb82ca0
- **From:** user
- **Target:** 0xAA6e91C82942aeAE040303Bf96c15a6dBcB82CA0
- **Value:** 54,917,637
- **Balance Increase:** 23
- **Index:** 1006201154718340586653299943

### Log Index 864 (0x360) - Repay
- **Contract:** 0x87870bca3f3fd6335c3f4ce8392d69350b4fa4e2 (Pool)
- **Reserve:** 0x1aBaEA1f7C830bD89Acc67eC4af516284b1bC33c (EURC)
- **User:** 0xc64d755e5FAF51A39F5CEE1e492dD9268b5dB203
- **Repayer:** 0xc64d755e5FAF51A39F5CEE1e492dD9268b5dB203
- **Amount:** 54,917,660
- **useATokens:** true

## Root Cause Analysis

The REPAY_WITH_ATOKENS operation involves:

1. **Interest accrual** (vToken Mint at logIndex 859): Interest is accrued on the debt position
2. **Debt repayment calculation**: The repayment amount is converted from normalized units to scaled units
3. **Debt burn**: The scaled amount is burned from the debt position
4. **Collateral burn**: aTokens are burned to cover the repayment

The 1 wei discrepancy occurs in step 3 - the debt burn calculation.

### Code Path Analysis

The REPAY_WITH_ATOKENS operation is processed by `_process_debt_mint_with_match()` since the first scaled event is a DEBT_MINT (interest accrual). Looking at the code:

```python
# From aave.py lines 3336-3380
def _process_debt_mint_with_match(...):
    # Check if this Mint event is part of a REPAY operation
    if operation.operation_type in {OperationType.REPAY, OperationType.GHO_REPAY}:
        # Treat as burn: calculate actual scaled burn amount from Pool event
        assert operation.pool_event is not None
        repay_amount, _ = eth_abi.abi.decode(
            types=["uint256", "bool"],
            data=operation.pool_event["data"],
        )
        token_math = TokenMathFactory.get_token_math_for_token_revision(
            debt_asset.v_token_revision
        )
        actual_scaled_burn = token_math.get_debt_burn_scaled_amount(
            repay_amount, scaled_event.index
        )
        # ... process burn with actual_scaled_burn
    else:
        # Treat as borrow/mint
        logger.debug("_process_debt_mint_with_match: handling as borrow/mint")
        _process_scaled_token_operation(
            event=DebtMintEvent(...),
            ...
        )
```

**THE BUG:** The condition on line 3336 checks for `OperationType.REPAY` and `OperationType.GHO_REPAY`, but **NOT** `OperationType.REPAY_WITH_ATOKENS`!

For REPAY_WITH_ATOKENS operations, the code falls through to the `else` branch (lines 3367-3380), which treats the DEBT_MINT event as a **regular borrow/mint** instead of calculating the actual scaled burn amount from the pool event.

This means:
1. The enrichment layer correctly calculates `scaled_amount = 54,441,954` (floor rounding of repay_amount)
2. But the processing layer ignores this and processes the Mint event as a borrow
3. The Mint event's `value=64,528,873` is treated as a debt increase instead of being ignored (since it's just interest accrual)
4. The actual debt burn of 54,441,954 is never applied

Wait, that doesn't match the 1 wei discrepancy. Let me reconsider...

Actually, the issue is more subtle. Looking at the actual transaction:
- The DEBT_MINT event at logIndex 859 has `value=64,528,873` and `balance_increase=119,446,532`
- In a REPAY operation where interest > repayment, the Mint event represents the **net debt increase** (interest - repayment), not the actual repayment
- The code should calculate the scaled burn from the **Repay event amount (54,917,660)**, not from the Mint event

For REPAY operations, the code does this correctly (lines 3336-3366). But for REPAY_WITH_ATOKENS, it doesn't - it falls through to treat the Mint as a borrow.

However, the balance difference is only 1 wei, not the full burn amount. This suggests that the enrichment layer's `scaled_amount` is being used somewhere, but there's still a 1 wei discrepancy.

Looking more carefully at line 3256:
```python
scaled_amount: int | None = enriched_event.scaled_amount
```

And line 3376:
```python
scaled_amount=scaled_amount,
```

So when the code falls through to the else branch for REPAY_WITH_ATOKENS, it does use `enriched_event.scaled_amount`. The enrichment layer should have calculated this correctly using the DEBT_BURN formula.

But wait - the enrichment layer has a special case for REPAY operations:
```python
# From enrichment.py lines 239-252
if (
    operation.operation_type in {OperationType.REPAY, OperationType.GHO_REPAY}
    and scaled_event.event_type in {ScaledTokenEventType.DEBT_MINT, ScaledTokenEventType.GHO_DEBT_MINT}
    and scaled_event.balance_increase is not None
):
    # Use DEBT_BURN for burn rounding (floor)
    calculation_event_type = ScaledTokenEventType.DEBT_BURN
```

**THE ROOT CAUSE:** The enrichment layer ALSO doesn't check for `OperationType.REPAY_WITH_ATOKENS`! It only checks for `REPAY` and `GHO_REPAY`.

This means for REPAY_WITH_ATOKENS operations:
1. The enrichment layer uses DEBT_MINT calculation (ceil rounding) instead of DEBT_BURN (floor rounding)
2. The processing layer treats it as a borrow/mint instead of a burn

The fix needs to add `OperationType.REPAY_WITH_ATOKENS` to both the enrichment layer and the processing layer checks.

## Proposed Fix

### Fix 1: Add REPAY_WITH_ATOKENS to Processing Layer

In `src/degenbot/cli/aave.py`, line 3336, add `OperationType.REPAY_WITH_ATOKENS` to the condition:

```python
# BEFORE:
if operation.operation_type in {OperationType.REPAY, OperationType.GHO_REPAY}:

# AFTER:
if operation.operation_type in {OperationType.REPAY, OperationType.GHO_REPAY, OperationType.REPAY_WITH_ATOKENS}:
```

### Fix 2: Add REPAY_WITH_ATOKENS to Enrichment Layer

In `src/degenbot/aave/enrichment.py`, line 239, add `OperationType.REPAY_WITH_ATOKENS` to the condition:

```python
# BEFORE:
if (
    operation.operation_type in {OperationType.REPAY, OperationType.GHO_REPAY}
    and scaled_event.event_type in {ScaledTokenEventType.DEBT_MINT, ScaledTokenEventType.GHO_DEBT_MINT}
    and scaled_event.balance_increase is not None
):

# AFTER:
if (
    operation.operation_type in {OperationType.REPAY, OperationType.GHO_REPAY, OperationType.REPAY_WITH_ATOKENS}
    and scaled_event.event_type in {ScaledTokenEventType.DEBT_MINT, ScaledTokenEventType.GHO_DEBT_MINT}
    and scaled_event.balance_increase is not None
):
```

## Key Insight

> **REPAY_WITH_ATOKENS was omitted from the special case handling for REPAY operations with DEBT_MINT events.**

When interest exceeds repayment, the VariableDebtToken emits a Mint event (instead of Burn) to represent the net debt increase. This happens for all REPAY operations, including REPAY_WITH_ATOKENS. The code that handles this special case only checked for REPAY and GHO_REPAY, causing REPAY_WITH_ATOKENS operations to be processed incorrectly.

This is a simple omission bug - the fix is to add REPAY_WITH_ATOKENS to the relevant condition checks.

## Files to Investigate

1. `src/degenbot/cli/aave.py`
   - `_process_debt_mint_with_match()` (lines 3191-3388)
   - Check how scaled_amount is calculated vs. enriched_event.scaled_amount

2. `src/degenbot/aave/enrichment.py`
   - Lines 181-252: DEBT_MINT handling for REPAY operations
   - Ensure scaled_amount calculation is correct

3. `src/degenbot/aave/libraries/wad_ray_math.py`
   - Verify `ray_div_floor` implementation matches Solidity exactly

## Testing Considerations

Test cases needed:
1. REPAY_WITH_ATOKENS with various repayment amounts
2. REPAY_WITH_ATOKENS where interest > repayment (DEBT_MINT case)
3. REPAY_WITH_ATOKENS with different EURC amounts
4. Standard REPAY operations to ensure no regression
5. Other assets to confirm this is not EURC-specific

## References

- `_process_debt_mint_with_match()` in aave.py:3336-3366
- `ExplicitRoundingMath.get_debt_burn_scaled_amount()` in token_math.py:197-200
- `ScaledEventEnricher.enrich()` in enrichment.py:64-300
- Issue 0037: REPAY Uses Mint Event Instead of Repay Event Amount
- Issue 0031: REPAY Debt Mint Validation Rounding Error

---

**Status:** ✅ FIXED

## Fix Implementation

### Changes Made

**File 1:** `src/degenbot/cli/aave.py` (line 3336)

Added `OperationType.REPAY_WITH_ATOKENS` to the condition that checks for REPAY operations with DEBT_MINT events:

```python
# BEFORE:
if operation.operation_type in {OperationType.REPAY, OperationType.GHO_REPAY}:

# AFTER:
if operation.operation_type in {
    OperationType.REPAY,
    OperationType.GHO_REPAY,
    OperationType.REPAY_WITH_ATOKENS,
}:
```

**File 2:** `src/degenbot/aave/enrichment.py` (line 239)

Added `OperationType.REPAY_WITH_ATOKENS` to the condition that determines when to use DEBT_BURN calculation for Mint events:

```python
# BEFORE:
operation.operation_type in {OperationType.REPAY, OperationType.GHO_REPAY}

# AFTER:
operation.operation_type
in {
    OperationType.REPAY,
    OperationType.GHO_REPAY,
    OperationType.REPAY_WITH_ATOKENS,
}
```

### Verification

After applying the fix:
- Transaction `0x4ea08cf2c8ddd47d18586e1cfedc7bd99cc64c534ef8056785d6bf4d747d6a83` processes correctly
- EURC debt balance for user `0xc64d755e5FAF51A39F5CEE1e492dD9268b5dB203`: 44,677,043,212 wei (matches contract)
- Aave market update completes successfully: "AaveV3Market successfully updated to block 23,245,734"

### Root Cause

The code that handles the special case of "interest exceeds repayment" (where a DEBT_MINT event is emitted instead of DEBT_BURN) only checked for `REPAY` and `GHO_REPAY` operation types. `REPAY_WITH_ATOKENS` was omitted, causing it to be processed incorrectly as a regular borrow/mint operation instead of calculating the actual scaled burn amount from the pool event.
