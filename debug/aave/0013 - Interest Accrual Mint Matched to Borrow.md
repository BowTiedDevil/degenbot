# Issue: Interest Accrual Mint Matched to Borrow Operation

**Date:** 2026-03-01

## Symptom

```
AssertionError: User 0x4bd5Eb24EB381DE15a168F213E16c32924Cd65D0: debt balance (998803871859734071459) does not match scaled token contract (2006613368907289127388) @ 0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B at block 18076682
```

## Root Cause

When a transaction contains multiple GHO debt mint events, including both an interest accrual mint (where `value == balance_increase`) and an actual borrow mint (where `value > balance_increase`), the `TransactionOperationsParser` incorrectly matched the interest accrual mint to the BORROW operation instead of the actual borrow mint.

### Technical Details

The `_create_borrow_operation` function in `aave_transaction_operations.py` was not filtering out interest accrual mints when matching debt mint events to BORROW operations. It would match the first GHO_DEBT_MINT event it encountered, regardless of whether it was interest accrual or an actual borrow.

Additionally, the `_create_interest_accrual_operations` function had an overly restrictive condition that only recognized interest accrual when `balance_increase > amount`, missing the case where `balance_increase == amount` (pure interest accrual).

## Transaction Details

- **Hash:** 0x1116737166520b7c1dfb24a1f42c135fd37179fa6e9b016dcaa16419930a0743
- **Block:** 18076682
- **Type:** GHO Borrow via mintGho
- **User:** 0x4bd5Eb24EB381DE15a168F213E16c32924Cd65D0
- **Asset:** GHO (0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f)
- **Amount:** 1000 GHO borrowed

### Event Sequence

1. **LogIndex 78:** Interest accrual mint (`value == balance_increase`, 0.97 GHO)
2. **LogIndex 106:** Zero-value mint (dust)
3. **LogIndex 112:** **Actual borrow mint** (`value > balance_increase`, 1010 GHO)
4. **LogIndex 114:** BORROW pool event

The code incorrectly matched the interest accrual mint (LogIndex 78) to the BORROW operation, ignoring the actual borrow mint (LogIndex 112).

## Fix

### Location 1: `_create_borrow_operation` (Line 924)

**File:** `src/degenbot/cli/aave_transaction_operations.py`

Added a check to skip mint events where `amount == balance_increase` (interest accrual):

```python
if ev.user_address == on_behalf_of:
    # Skip interest accrual mints (amount == balance_increase)
    # These should be handled by INTEREST_ACCRUAL operations
    # Only match actual borrow mints (amount > balance_increase)
    if ev.amount == ev.balance_increase:
        continue
    # ... rest of matching logic
```

### Location 2: `_create_interest_accrual_operations` (Line 1306)

**File:** `src/degenbot/cli/aave_transaction_operations.py`

Changed the condition from `balance_increase > amount` to `balance_increase >= amount` to recognize pure interest accrual:

```python
# Interest accrual: balance_increase >= amount
# - balance_increase > amount: net interest after repayment (in _burnScaled)
# - balance_increase == amount: pure interest accrual (in _accrueDebtOnAction)
is_interest_accrual = ev.balance_increase >= ev.amount
```

## Key Insight

When matching scaled token events to pool operations, it's crucial to distinguish between:
- **Interest accrual mints:** `value == balance_increase` (or `balance_increase > value` for net interest)
- **Actual borrow mints:** `value > balance_increase`

The parser must skip interest accrual mints when matching BORROW operations to ensure the correct scaled amount is applied to user balances.

## Refactoring

The event matching logic in `TransactionOperationsParser` could be made more robust by:

1. **Explicit event classification:** Add a helper method to classify mint events as "interest accrual", "borrow", or "repay with net interest"
2. **Stricter matching criteria:** Consider not just the user address but also the mint type when matching to pool events
3. **Validation:** Add assertions that verify the matched mint event type aligns with the pool operation type

## References

- Transaction: https://etherscan.io/tx/0x1116737166520b7c1dfb24a1f42c135fd37179fa6e9b016dcaa16419930a0743
- Test: `tests/cli/test_aave_transaction_operations.py::TestBug0013InterestAccrualMintMatching`
