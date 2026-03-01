# Issue 0011: GHO Dust Mint Events Not Processed

**Date:** 2025-02-27

## Symptom

```
AssertionError: User 0x0fd3E4B5FcaC38ba6E48e9c7703805679eDFCcC4: debt last_index (1000919640646688461729030319) does not match contract (1000919954862378321350351390) @ 0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B at block 17859071
```

## Root Cause

GHO VariableDebtToken emits `Mint` events during `updateDiscountDistribution` calls when stkAAVE balances change. These events have `amount=0` and `balance_increase=0` ("dust mints") but still need to update the user's `last_index`.

The operation-based event processing logic in `aave_transaction_operations.py` was rejecting these events:

1. **Line 1278**: Only created `INTEREST_ACCRUAL` operations for mints with `balance_increase > 0`
2. **Lines 1719-1723**: Validation rejected `INTEREST_ACCRUAL` operations with `balance_increase == 0`

This meant dust mints were never assigned to any operation and never processed, leaving the database's `last_index` stale.

## Transaction Details

| Field | Value |
|-------|-------|
| **Transaction Hash** | 0x7120d824085292eafa6d540a17386f4a09168c658d17ea47d2705cd002a81636 |
| **Block** | 17859071 |
| **Type** | Uniswap swap + stkAAVE transfer + GHO discount update |
| **User** | 0x0fd3E4B5FcaC38ba6E48e9c7703805679eDFCcC4 |
| **Asset** | GHO Variable Debt Token (0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B) |

### Event Details

The transaction included a `Mint` event from `updateDiscountDistribution`:
- **Caller:** 0x0000000000000000000000000000000000000000 (Zero Address)
- **On Behalf Of:** 0x0fd3E4B5FcaC38ba6E48e9c7703805679eDFCcC4
- **Value:** 0
- **Balance Increase:** 0
- **Index:** 1000919954862378321350351390

This is a valid "dust mint" that updates the user's cached `lastIndex` during discount recalculation but doesn't change the actual debt balance.

## Fix

### File: `src/degenbot/cli/aave_transaction_operations.py`

**Change 1:** Remove the `balance_increase > 0` requirement when creating INTEREST_ACCRUAL operations (lines 1274-1309):

```python
# Before:
if ev.balance_increase > 0:
    # ... create INTEREST_ACCRUAL operation

# After:
# Process all unassigned mint events (including dust mints with balance_increase == 0)
# ... create INTEREST_ACCRUAL operation
```

**Change 2:** Update validation to allow dust mints (lines 1700-1725):

```python
# Before:
if ev.balance_increase == 0:
    errors.append(
        f"INTEREST_ACCRUAL event should have balance_increase > 0, "
        f"got balance_increase={ev.balance_increase}"
    )

# After:
# Allow both interest accrual (balance_increase > 0) and dust mints (balance_increase == 0)
# Dust mints occur during discount updates and still need to update last_index
```

## Key Insight

Not all mint events represent balance changes. In Aave V3:
- **Interest accrual:** `balance_increase > 0` - increases user balance
- **Dust mints:** `balance_increase == 0` - updates user's cached `lastIndex` only

Dust mints occur during:
- Discount rate updates (`updateDiscountDistribution`)
- Internal accounting adjustments
- Staking reward distribution hooks

These events must still be processed to keep `last_index` synchronized with the contract.

## Refactoring

Consider renaming `INTEREST_ACCRUAL` to something more generic like `STANDALONE_MINT` or `INDEX_UPDATE` to better reflect that it handles:
1. Pure interest accrual (balance_increase > 0)
2. Dust mints from discount updates (balance_increase == 0)
3. Other standalone mint events without pool operations

Alternatively, split into separate operation types:
- `INTEREST_ACCRUAL` - for balance-increasing mints
- `DISCOUNT_UPDATE` - for dust mints from discount changes

## Verification

After the fix:
```bash
uv run degenbot aave update --to-block=17859071
# Successfully processes the transaction and updates last_index
```

All 149 existing tests pass.
