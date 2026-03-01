# Issue: Interest Accrual Mint Event Skipped in Flash Loan Transactions

**Date:** 2025-03-01

**Symptom:**
```
AssertionError: User 0xC5Ec4153F98729f4eaf61013B54B704Eb282ECF4: debt balance (68367319763) does not match scaled token contract (68362424888) @ 0x72E95b8931767C79bA4EeE721354d6E99a61D004 at block 17996836
```

**Root Cause:**
When processing flash loan transactions that involve debt swaps, the code was incorrectly skipping DEBT_MINT events when `has_borrow` was True. This caused interest accrual Mint events (where `balance_increase > amount`) to be skipped during flash loan transactions.

In the failing transaction:
1. User borrowed BAL via flash loan
2. Swapped BAL for USDC
3. Repaid USDC debt
4. During repayment, interest accrued (balance_increase=44,551,487 > amount=39,551,487)
5. The Mint event representing this interest accrual was skipped
6. Result: Database debt balance was 4,894,875 higher than contract

**Transaction Details:**
- **Hash:** 0xa044d93a1aced198395d3293d4456fcb09a9a734d2949b5e2dff66338fa89625
- **Block:** 17996836
- **Type:** Flash Loan Debt Swap
- **User:** 0xC5Ec4153F98729f4eaf61013B54B704Eb282ECF4
- **Asset:** USDC (variable debt)
- **vToken:** 0x72E95b8931767C79bA4EeE721354d6E99a61D004
- **Flash Loan:** 1,090,963,540 BAL
- **USDC Repayment:** 5,000,000 USDC
- **Interest Accrued:** 44,551,487 (scaled)
- **Net Minted:** 39,551,487 (scaled)
- **Discrepancy:** 4,894,875 (scaled)

**Events in Transaction:**
1. Borrow (BAL flash loan) - topics[0] = 0xb3d08482...
2. Mint (USDC debt, interest accrual) - topics[0] = 0x458f5fa4...
3. Repay (USDC) - topics[0] = 0xa534c8db...
4. Transfer (USDC)
5. Mint (USDC debt, second interest accrual)
6. ReserveDataUpdated
7. Borrow (BAL)
8. Transfer
9. Burn (BAL debt)
10. ReserveDataUpdated
11. Repay (BAL)
12. Transfer

**Fix:**
File: `src/degenbot/cli/aave_transaction_operations.py` (lines 1294-1301)

Changed from:
```python
# Skip DEBT_MINT in liquidation/borrow transactions - these may be
# flash borrows. For REPAY transactions, only skip if there's no
# collateral burn (if there is, it's repayWithATokens and DEBT_MINT
# should become INTEREST_ACCRUAL).
if ev.event_type in {"DEBT_MINT", "GHO_DEBT_MINT"}:
    if has_liquidation or has_borrow:
        continue
    # Skip DEBT_MINT during REPAY only if it's not interest accrual
    # Interest accrual during repayment: balance_increase > amount
    # This occurs in _burnScaled when interest > repayment amount
    if has_repay and not has_collateral_burn and ev.balance_increase <= ev.amount:
        continue
```

To:
```python
# Skip DEBT_MINT in liquidation transactions - these may be
# protocol operations that should not create INTEREST_ACCRUAL.
# For borrow/repay transactions, only skip if it's not interest
# accrual (balance_increase > amount). Interest accrual can occur
# during any transaction type including flash loans.
if ev.event_type in {"DEBT_MINT", "GHO_DEBT_MINT"}:
    if has_liquidation:
        continue
    # Interest accrual: balance_increase > amount (net interest after repayment)
    # This occurs in _burnScaled when interest > repayment amount
    # Always process interest accrual, regardless of transaction type
    is_interest_accrual = ev.balance_increase > ev.amount
    if is_interest_accrual:
        # Process as INTEREST_ACCRUAL
        pass
    elif has_borrow:
        # Skip DEBT_MINT during borrow (flash loan) if not interest accrual
        continue
    elif has_repay and not has_collateral_burn:
        # Skip DEBT_MINT during REPAY if not interest accrual
        continue
```

**Key Changes:**
1. Removed `has_borrow` from the first condition that skips all DEBT_MINT events
2. Added explicit check for `is_interest_accrual = ev.balance_increase > ev.amount`
3. Interest accrual events are now processed regardless of transaction type (borrow, repay, etc.)
4. Non-interest DEBT_MINT events are still skipped during borrow/repay as before

**Key Insight:**
Interest accrual during debt repayment is a separate concern from the transaction type (flash loan, regular borrow, etc.). The Mint event should be processed as INTEREST_ACCRUAL whenever `balance_increase > amount`, regardless of whether there's also a BORROW event in the same transaction.

**Refactoring:**
The current logic for skipping DEBT_MINT events has grown complex with multiple conditions. A cleaner approach would be:
1. Categorize Mint events based on their properties first
2. Always process interest accrual (balance_increase > amount) as INTEREST_ACCRUAL
3. Skip non-interest DEBT_MINT events during borrow/repay/liquidation operations
4. Remove the dependency on has_repay/has_borrow flags for interest accrual processing

This would make the code more maintainable and less prone to similar bugs.

**Verification:**
- Full AAVE update completes successfully to block 17996836
- All 151 AAVE-related tests pass
- Verified transaction 0xa044d9... now processes correctly
- Database balance now matches on-chain: 68362424888

**Related Issues:**
- Issue 0003: Interest Accrual During Repayment Not Processed (similar fix for REPAY transactions)
- This issue extends that fix to also handle BORROW transactions (flash loans)
