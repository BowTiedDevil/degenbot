# Aave Debug Progress

## Issue: BalanceTransfer Skipped After Pure Interest Mint

**Date:** 2025-02-20

**Symptom:**
```
AssertionError: User 0xD2eEe629994e83194Db1D59cFCf9eaa923C8e110: collateral balance (46303) does not match scaled token contract (46318) @ 0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c at block 22010198
```

**Root Cause:**
The `_process_scaled_token_balance_transfer_event()` function in `src/degenbot/cli/aave.py` incorrectly skipped BalanceTransfer events when they followed a Mint event with `value == balanceIncrease` (pure interest accrual). The skip condition was:

```python
if prior_value == prior_balance_increase:  # BUG: This is backwards!
    skip_to_user_balance_update = True
```

This logic assumed that when `value == balanceIncrease`, the Mint had already added the balance. However, the opposite is true:

- When `value == balanceIncrease`: It's pure interest accrual. The Mint processor returns `balance_delta = 0` (no balance change). The BalanceTransfer MUST still be processed.
- When `value != balanceIncrease`: It's an actual deposit (SUPPLY operation). The Mint processor adds the `balance_delta = value`. The BalanceTransfer should be skipped because the Mint already added it.

The condition was backwards, causing the BalanceTransfer to be skipped when it should have been processed, losing 15 tokens from the recipient's balance.

**Transaction Details:**
- **Hash:** 0xb4dd38f135d8ceddb73466cabe6da17af9f717a5b40393ca8a67208523360f5a
- **Block:** 22010198
- **Type:** Interest Accrual + Balance Transfer
- **User:** 0xD2eEe629994e83194Db1D59cFCf9eaa923C8e110
- **Asset:** aEthUSDC (0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c)
- **Events:**
  - Log 104: Mint event with `value = balanceIncrease = 15` (pure interest accrual)
  - Log 107: BalanceTransfer event transferring 15 tokens to recipient

**Fix:**
Updated `src/degenbot/cli/aave.py` line 4232, changing the skip condition from:

```python
prior_value == prior_balance_increase  # Pure interest accrual - BUG!
```

To:

```python
prior_value != prior_balance_increase  # Not pure interest (actual deposit)
```

Also updated the explanatory comment at lines 4215-4219 to clarify the logic.

**Key Insight:**
Collateral Mint events have two distinct behaviors based on the relationship between `value` and `balanceIncrease`:

1. **Pure Interest Accrual** (`value == balanceIncrease`): Emitted from `_accrueInterestOnAction()`. Returns `balance_delta = 0`.
2. **Actual Deposit** (`value != balanceIncrease`): Emitted from `SUPPLY` or `REPAY` with aTokens. Returns `balance_delta = value`.

The BalanceTransfer skip logic should only apply when the Mint is an actual deposit (case 2), not when it's pure interest accrual (case 1).

**Refactoring:**
Consider extracting the Mint event classification logic (`value == balanceIncrease` vs `value != balanceIncrease`) into a shared helper function or enum to make the code more self-documenting and prevent similar bugs. The classification is used in multiple places:
- Mint event processing (determining balance_delta)
- BalanceTransfer event processing (skip logic)
- Event matching logic (finding corresponding Pool events)

A clear abstraction like `is_pure_interest_accrual(value, balance_increase)` would improve readability and reduce the risk of inverted logic bugs.
