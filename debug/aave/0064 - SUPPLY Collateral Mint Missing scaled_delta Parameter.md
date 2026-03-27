# 0064 - SUPPLY Collateral Mint Missing scaled_delta Parameter

**Issue:** 1 Wei Rounding Error in SUPPLY Collateral Balance Verification

**Date:** 2026-03-26

## Symptom

```
AssertionError: Collateral balance verification failure for AaveV3Asset(...).
User 0x246E20bF778b3e16cB71eca535f40f8C4E6c4185 scaled balance (9452400556120464)
does not match contract balance (9452400556120465) at block 24247936

Difference: 1 wei
```

## Root Cause

**This is a regression caused by commit `b7ffe8b4`.**

The timeline:
- **March 21, 2026**: Issue 0049 identified and fixed in commit `1f999b2e` (added `scaled_delta=event.scaled_amount` parameter)
- **March 25, 2026**: Fix accidentally reverted in commit `b7ffe8b4` ("lint: cleanup" removed the line)
- **March 26, 2026**: Bug manifested again at block 24247936

The bug is in `src/degenbot/cli/aave.py` in the `_process_scaled_token_operation()` function. When processing `CollateralMintEvent` (lines 2486-2502), the code does NOT pass the pre-calculated `scaled_amount` from the enrichment layer to the processor. Instead, the processor recalculates the scaled amount from the Mint event's fields.

### Why This Causes a 1 Wei Error

**Supply Transaction Flow:**
1. User supplies: 10,000,000,000,000,000 wei (0.01 WETH)
2. Pool calculates scaled amount: `floor(10000000000000000 * RAY / index)` = **9452400556120465**
3. Pool mints 9452400556120465 scaled aTokens
4. Mint event displays: `floor(9452400556120465 * index / RAY)` = **9999999999999999** (1 wei less due to rounding)

**The Bug:**
- Enrichment layer correctly calculates scaled_amount=9452400556120465 from the Supply event
- But processing layer calls `process_mint_event()` without `scaled_delta` parameter
- Processor recalculates from Mint event: `floor(9999999999999999 * RAY / index)` = **9452400556120464** (1 wei less!)

### Pattern Inconsistency

All other event types correctly pass `scaled_delta`:
- `CollateralBurnEvent` (line 2513): ✓ passes `scaled_delta=event.scaled_amount`
- `DebtMintEvent` (line 2534): ✓ passes `scaled_delta=event.scaled_amount`
- `DebtBurnEvent` (line 2551): ✓ passes `scaled_delta=event.scaled_amount`
- `CollateralMintEvent` (line 2495): ✗ **MISSING** - causes the bug

## Transaction Details

- **Hash:** `0x55a9a6541dca588f54989edf9747db92f30566e1f9863cfc95a16960a6f97c24`
- **Block:** 24247936
- **Type:** SUPPLY
- **User:** `0x246E20bF778b3e16cB71eca535f40f8C4E6c4185`
- **Asset:** WETH (aWETH: 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8)
- **Pool Revision:** 10
- **Token Revisions:** aToken=5, vToken=5
- **Supply Amount:** 10,000,000,000,000,000 wei
- **Liquidity Index:** 1057932314720302672289370287

## Fix

**File:** `src/degenbot/cli/aave.py`
**Function:** `_process_scaled_token_operation()`
**Line:** 2491-2495

**Change:**
```python
# BEFORE (buggy):
mint_result: ScaledTokenMintResult = collateral_processor.process_mint_event(
    event_data=event,
    previous_balance=position.balance,
    previous_index=position.last_index or 0,
)

# AFTER (fixed):
mint_result: ScaledTokenMintResult = collateral_processor.process_mint_event(
    event_data=event,
    previous_balance=position.balance,
    previous_index=position.last_index or 0,
    scaled_delta=event.scaled_amount,  # <-- ADD THIS LINE
)
```

This fix aligns `CollateralMintEvent` processing with the established pattern used for all other event types.

## Key Insight

> **The enrichment layer's `scaled_amount` must be used as the authoritative value for balance updates. Recalculating from Mint event fields introduces rounding errors because the Mint event's `amount` field is already a display amount that has undergone rounding transformations.**

## Alternatives Considered

**Alternative 1: Modify the processor logic**
- Change `v5.py` to always use `event.scaled_amount` when available
- More invasive change
- Less flexible for edge cases

**Alternative 2: Add tolerance in verification**
- Rejected per instructions - amounts must match exactly

**Alternative 3: Use Mint event amount directly**
- Would still have 1 wei discrepancy
- Doesn't fix root cause

The chosen fix is **minimal, follows existing patterns, and addresses the root cause**.

## Why Did This Take "8 Million Blocks" to Manifest?

**Short answer: It didn't.**

The bug is not a latent issue that existed for millions of blocks. It was introduced on March 25, 2026 by commit `b7ffe8b4` which accidentally removed the fix from commit `1f999b2e` (March 21, 2026) during a "lint cleanup."

**Timeline:**
1. **March 21**: Fix implemented in commit `1f999b2e`
2. **March 25**: Fix accidentally removed in commit `b7ffe8b4`
3. **March 26**: Bug manifested at block 24247936 (the next update after the regression)

**The Lesson:** Lint and cleanup commits should be reviewed as carefully as feature commits. Automated refactoring can inadvertently remove critical fixes.

## Refactoring

1. **Consistency:** All event processing should follow the same pattern: enrichment calculates, processing uses `scaled_delta` parameter.

2. **Code Review:** This bug suggests the need for a code audit to ensure all event types use consistent parameter passing.

3. **Documentation:** Add a comment explaining why `scaled_delta` is required for SUPPLY operations to prevent future regressions.

---

**Status:** ✅ FIXED AND VERIFIED

**Fix Applied:** Added `scaled_delta=event.scaled_amount` parameter to `process_mint_event()` call in `src/degenbot/cli/aave.py:2495`

**Also Updated:** Protocol definition in `src/degenbot/aave/processors/base.py:205-210` to include `scaled_delta` parameter

**Verification:** Aave update successfully processes block 24247936 without assertion error

**Related Issues:** 0049 - SUPPLY Collateral Mint Uses Mint Event Amount Instead of Supply Amount
