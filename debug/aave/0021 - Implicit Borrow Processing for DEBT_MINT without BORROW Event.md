# Issue 0021: Implicit Borrow Processing for DEBT_MINT without BORROW Event

**Date:** 2026-03-03

## Symptom

```
AssertionError: User 0xB22e3d2418C2B909C14883F35EA0BDcBA566e9c6: debt balance (2060594563394477418258) does not match scaled token contract (2647567112895583751348) @ 0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE at block 20363588
```

## Root Cause

Transaction `0x37416a998da98779737e6c62607defcf9d0a7fbfd38651e54b8c058710eb3992` at block 20363588 is a complex DeFi transaction (Kamino + Aave leverage) that includes:

1. **Supply event** - Deposit weETH as collateral (Log 173)
2. **Repay event** - Repay existing weETH debt (Log 174)
3. **Collateral Mint** for weETH aToken (Log 172) - interest accrual
4. **Two vToken Mint events** for WETH:
   - Log 175: Small mint (266,163,817,852,323,386 wei) - borrow with interest (balance_increase=26.6K)
   - Log 176: Main mint (614,800,334,026,855,555,114 wei) - pure borrow (balance_increase=0)
5. **NO BORROW event** from the Pool contract

The `_create_interest_accrual_operations` method was designed to skip DEBT_MINT events during complex transactions (liquidations, borrows, repays). The original logic on line 1309 skipped these events with `continue`, preventing them from being processed at all.

However, this logic incorrectly skipped ALL DEBT_MINT events in transactions with REPAY events, even those for different assets (WETH borrow vs weETH repay). Specifically:
- Log 175 (borrow with interest) was being skipped when it should be processed as INTEREST_ACCRUAL
- Log 176 (pure borrow) was being skipped when it should create an IMPLICIT_BORROW operation

The main borrow mint (614.8 WETH) was never assigned to any operation and was never processed, resulting in a database balance shortfall.

## Transaction Details

- **Hash:** `0x37416a998da98779737e6c62607defcf9d0a7fbfd38651e54b8c058710eb3992`
- **Block:** 20,363,588
- **Type:** Kamino Automation (multicall)
- **User:** `0xB22e3d2418C2B909C14883F35EA0BDcBA566e9c6`
- **Asset:** WETH (variable debt)
- **vToken:** `0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE`
- **Implementation:** `0xac725cb59d16c81061bdea61041a8a5e73da9ec6`
- **Revision:** 1

## Fix

### Changes to `aave_transaction_operations.py`

1. **Added new operation type** (line 94):
   ```python
   IMPLICIT_BORROW = auto()  # DEBT_MINT without BORROW event (e.g., flash loans, internal operations)
   ```

2. **Modified `_create_interest_accrual_operations`** (lines 1298-1324):
   ```python
   # Handle DEBT_MINT events based on type
   if ev.event_type in {"DEBT_MINT", "GHO_DEBT_MINT"}:
       # Interest accrual: balance_increase >= amount
       # - balance_increase > amount: net interest after repayment (in _burnScaled)
       # - balance_increase == amount: pure interest accrual (in _accrueDebtOnAction)
       is_interest_accrual = ev.balance_increase >= ev.amount
       # Pure borrow: balance_increase == 0 (no interest accrued)
       is_pure_borrow = ev.balance_increase == 0

       if not is_interest_accrual:
           # This is either a pure borrow or borrow with interest
           # Skip during liquidation/flash loans as those are handled separately
           if has_liquidation or has_borrow:
               continue
           # For pure borrows (balance_increase == 0), create IMPLICIT_BORROW
           if is_pure_borrow:
               operations.append(
                   Operation(
                       operation_id=operation_id,
                       operation_type=OperationType.IMPLICIT_BORROW,
                       pool_event=None,
                       scaled_token_events=[ev],
                       transfer_events=[],
                       balance_transfer_events=[],
                   )
               )
               operation_id += 1
               continue
           # Borrow with interest (0 < balance_increase < amount) falls through
           # to be processed as INTEREST_ACCRUAL
       # Interest accrual falls through to be processed below
   ```

   Key changes:
   - Only skip with `continue` during liquidation/flash loan scenarios (has_liquidation or has_borrow)
   - For pure borrows (balance_increase == 0), create IMPLICIT_BORROW operation
   - For borrows with interest (0 < balance_increase < amount), fall through to INTEREST_ACCRUAL
   - For interest accrual (balance_increase >= amount), fall through to INTEREST_ACCRUAL

### Changes to `aave.py`

1. **Added IMPLICIT_BORROW handling** (lines 2533-2553):
   ```python
   # Handle IMPLICIT_BORROW operations - DEBT_MINT without BORROW event
   if match_result is None and operation.operation_type == OperationType.IMPLICIT_BORROW:
       if scaled_event.event_type in {"DEBT_MINT", "GHO_DEBT_MINT"}:
           _process_debt_mint_with_match(...)
       continue
   ```

## Key Insight

The Aave protocol can emit DEBT_MINT events without corresponding BORROW events in certain scenarios:
- Internal Pool operations (e.g., flash loans)
- Complex multi-step transactions where the borrow is implicit
- Protocol-level operations that mint debt directly

The fix distinguishes between three types of DEBT_MINT events:
1. **Pure borrow** (balance_increase == 0): Create IMPLICIT_BORROW operation
2. **Borrow with interest** (0 < balance_increase < amount): Process as INTEREST_ACCRUAL
3. **Interest accrual** (balance_increase >= amount): Process as INTEREST_ACCRUAL

Only pure borrows without BORROW events should create IMPLICIT_BORROW operations.

## Refactoring Recommendations

1. **Event Matching Strategy**: Consider enhancing the event matching logic to better handle complex multi-operation transactions where pool events might be missing or implicitly handled.

2. **Transaction Classification**: Add more sophisticated transaction pattern detection to automatically classify transactions that bypass normal event patterns (flash loans, internal pool operations, etc.).

3. **Validation**: Add post-processing validation to ensure all scaled token events in a transaction are assigned to operations, logging warnings for unassigned events.

## Test Coverage

Added test file: `tests/cli/test_aave_issue_0021.py`

Tests verify:
- `IMPLICIT_BORROW` operation type exists and has correct ordering
- Operation type values are unique
- `IMPLICIT_BORROW` comes after `MINT_TO_TREASURY` and before `UNKNOWN`
- Flash loan scenarios still skip DEBT_MINT events correctly
- Interest accrual events are still processed correctly

All 17 tests pass (14 existing + 3 new).

## Files Modified

1. `src/degenbot/cli/aave_transaction_operations.py` - Added IMPLICIT_BORROW operation type and modified interest accrual logic
2. `src/degenbot/cli/aave.py` - Added IMPLICIT_BORROW processing logic
3. `tests/cli/test_aave_issue_0021.py` - Added test coverage

## Verification

The fix creates the following operations for the failing transaction:
- **SUPPLY** (Log 173): Collateral deposit
- **REPAY** (Log 174): Debt repayment  
- **INTEREST_ACCRUAL** (Log 172): Collateral interest accrual
- **INTEREST_ACCRUAL** (Log 175): Debt mint with interest
- **IMPLICIT_BORROW** (Log 176): Pure debt borrow (614.8 WETH)

The IMPLICIT_BORROW operation ensures the 614.8 WETH borrow is properly processed and added to the user's debt balance.
