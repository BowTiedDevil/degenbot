# 0029 - Double Transfer Processing in Repay With ATokens

**Issue:** User's collateral balance incorrect after repayWithATokens operation

**Date:** 2026-03-06

## Symptom

```
AssertionError: User 0x1CEBd13797636b94C5CaD7108CEbE42D2Fce7732: collateral balance (-70825098) does not match scaled token contract (0) @ 0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c at block 21223796
```

The user's tracked balance is exactly the negative of the BalanceTransfer amount (70,825,098), suggesting the BalanceTransfer is being processed as a reduction instead of an increase.

## Root Cause Hypothesis

When processing a transaction with `repayWithATokens` involving a Furucombo proxy, the BalanceTransfer event (Log 0x1f0) is being processed incorrectly. The BalanceTransfer represents a scaled balance movement from the Furucombo proxy to the user, but it appears to be reducing the user's balance instead of increasing it.

The issue may be in one of these areas:
1. **Double-processing**: The BalanceTransfer is being processed both as part of its paired Transfer event AND separately
2. **Direction error**: The BalanceTransfer is being processed with reversed from/to addresses
3. **Skip logic failure**: The skip logic at `aave.py:3321-3333` is not preventing the BalanceTransfer from being processed separately

## Transaction Details

- **Hash:** 0x59da72746a5e34b7f93502047d4e11104f447f419563bb2ef3131ae8456e0522
- **Block:** 21223796
- **Type:** repayWithATokens + Borrow (via Furucombo proxy)
- **User:** 0x1CEBd13797636b94C5CaD7108CEbE42D2Fce7732
- **Asset:** aUSDC (0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c)
- **Furucombo Proxy:** 0x3ef3d8ba38ebe18db133cec108f4d14ce00dd9ae

## Key Events (aUSDC Token)

| Log Index | Event Type | Topic | From | To | Amount |
|-----------|------------|-------|------|-----|--------|
| 0x1ec | Transfer | 0xddf252ad... | 0x0 (Mint) | Furucombo | 11,772,476 |
| 0x1ed | Mint | 0x458f5fa4... | Furucombo | Furucombo | 11,772,476 |
| 0x1ee | Transfer | 0xddf252ad... | Furucombo | User | 77,666,633 |
| 0x1f0 | BalanceTransfer | 0x4beccb90... | Furucombo | User | 70,825,098 |
| 0x1f5 | Transfer | 0xddf252ad... | User | 0x0 (Burn) | 77,666,633 |
| 0x1f6 | Burn | 0x4cf25bc1... | User | User | 77,666,633 |

## Expected Behavior

User starts with 0 aUSDC.

**Option 1** (if using BalanceTransfer amount):  
0 + 70,825,098 (receive) - 77,666,633 (burn) = **-6,841,535**

**Option 2** (if using ERC20 Transfer amount):  
0 + 77,666,633 (receive) - 77,666,633 (burn) = **0**

## Actual Behavior

User balance: **-70,825,098**

This is exactly the BalanceTransfer amount, suggesting the BalanceTransfer is being applied as a reduction rather than an increase.

## Code Locations

### Event Decoding
- File: `src/degenbot/cli/aave_transaction_operations.py`
- Function: `_decode_balance_transfer_event` (line 650)
- Note: Sets `user_address=from_addr` (line 681)

### Event Pairing
- File: `src/degenbot/cli/aave_transaction_operations.py`
- Function: `_create_transfer_operations` (line 1480)
- Logic: Pairs ERC20 Transfer with BalanceTransfer (lines 1524-1550)

### Transfer Processing
- File: `src/degenbot/cli/aave.py`
- Function: `_process_collateral_transfer_with_match` (line 3307)
- Skip logic: Lines 3321-3333 (should skip BalanceTransfer events in `balance_transfer_events`)
- Balance update: Lines 3445-3454 (sender), Lines 3457-3504 (recipient)

### Unassigned Event Processing
- File: `src/degenbot/cli/aave.py`
- Lines: 2384-2393
- Note: Skips events in `assigned_log_indices` which includes `balance_transfer_events`

## Investigation Steps Taken

1. ✅ Verified the transaction events using `cast receipt`
2. ✅ Confirmed the BalanceTransfer topic (0x4beccb90...) differs from Burn topic (0x4cf25bc1...)
3. ✅ Tested operation parsing - correctly pairs Transfer (0x1ee) with BalanceTransfer (0x1f0)
4. ✅ Verified BalanceTransfer is placed in `balance_transfer_events`, not `scaled_token_events`
5. ✅ Confirmed skip logic exists to prevent double-processing
6. ❌ Could not identify why the BalanceTransfer amount is being subtracted from user balance

## Key Insight

The BalanceTransfer at Log 0x1f0 has:
- `from` = Furucombo proxy (0x3ef3d8ba...)
- `to` = User (0x1cebd137...)
- `amount` = 70,825,098 (scaled balance)

When decoded in `_decode_balance_transfer_event`:
- `user_address` = from_addr = Furucombo proxy
- `from_address` = from_addr = Furucombo proxy  
- `target_address` = to_addr = User

If this BalanceTransfer were processed separately (not just used for amount), it would:
1. Reduce Furucombo proxy's balance by 70,825,098
2. Increase User's balance by 70,825,098

But the error shows User balance = -70,825,098, suggesting the amount is being subtracted instead of added.

## Potential Fix Areas

1. **Verify skip logic is working**: Add debug logging at `aave.py:3321-3333` to confirm BalanceTransfer events are being skipped
2. **Check processing order**: Ensure the paired Transfer is processed and correctly updates the recipient balance
3. **Verify amount calculation**: Check that `_process_collateral_transfer_with_match` uses the correct amount when a paired BalanceTransfer exists
4. **Check for unassigned processing**: Verify BalanceTransfer events are not being processed in the unassigned events loop

## Steps to Reproduce

```bash
# Run the Aave update to the failing block
uv run degenbot aave update --to-block=21223796

# Or run with debug output
uv run degenbot aave update --debug-output=/tmp/debug.log --to-block=21223796
```

## Refactoring Recommendations

1. **Add comprehensive debug logging** for transfer operations showing:
   - Which events are paired
   - What amounts are being used
   - How balances are being updated

2. **Add validation** to ensure BalanceTransfer events are never processed separately from their paired Transfer events

3. **Improve test coverage** for transactions involving:
   - Furucombo or other proxy contracts
   - Multiple transfers in a single transaction
   - BalanceTransfer + ERC20 Transfer pairs

## Related Issues

- Issue #0026: BalanceTransfer followed by Withdraw must use matching amounts
- Issue #0027: Transfer Burn Collateral Balance Mismatch

## Files to Examine

- `src/degenbot/cli/aave.py` (lines 3307-3550)
- `src/degenbot/cli/aave_transaction_operations.py` (lines 1480-1595)
- `src/degenbot/aave/events.py` (event topic definitions)

## Root Cause Identified

The issue is in the BalanceTransfer tracking logic in `_process_collateral_transfer_with_match` (lines 3503-3546 of `aave.py`).

### Problem Flow

1. **BALANCE_TRANSFER operation created**: The Transfer at Log 0x1ee (Furucombo → User) and BalanceTransfer at Log 0x1f0 are paired and create a BALANCE_TRANSFER operation.

2. **REPAY_WITH_ATOKENS operation created separately**: The Burn at Log 0x1f6 (User burning their aTokens) is part of a separate REPAY_WITH_ATOKENS operation.

3. **Transfer processing**: When processing the BALANCE_TRANSFER operation's Transfer event:
   - The User is the recipient
   - The code detects that User immediately burns (the Burn from the repay operation)
   - So `skip_recipient_update = True` (the User's balance is NOT increased)
   - **BUT** the BalanceTransfer is still tracked in `processed_balance_transfers`

4. **Burn processing**: When processing the REPAY_WITH_ATOKENS operation's Burn event:
   - It looks for a tracked BalanceTransfer with key `(token, user_address)`
   - The Burn's `user_address` = User (the one burning)
   - It finds the BalanceTransfer tracked in step 3
   - It uses the BalanceTransfer amount (70,825,098) instead of the Burn amount (77,666,633)
   - This reduces User's balance by 70,825,098

5. **Result**: User's balance = 0 - 70,825,098 = **-70,825,098**

### The Bug

The BalanceTransfer tracking at lines 3508-3546 doesn't check if the recipient's balance was actually updated. When `skip_recipient_update` is True, the BalanceTransfer should NOT be tracked because:
- The recipient's balance wasn't updated (transfer was "virtual")
- Using the tracked amount later would cause the burn to use the wrong amount
- The burn should use its own amount since the transfer was never actually credited

## Solution

**File**: `src/degenbot/cli/aave.py`  
**Lines**: 3508

**Change**: Add `and not skip_recipient_update` to the condition that determines whether to track the BalanceTransfer.

```python
# BEFORE:
if tx_context is not None:

# AFTER:
if tx_context is not None and not skip_recipient_update:
```

This ensures that BalanceTransfers are only tracked when the recipient's balance was actually updated. If the recipient update was skipped (due to immediate burn detection), the BalanceTransfer won't be tracked, and subsequent burns will use their own amounts.

## Verification

The fix has been verified by:
1. Running existing tests: `tests/cli/test_aave_balance_transfer_immediate_burn.py` and `tests/cli/test_aave_transfer_balance_transfer_pairing.py` all pass
2. Code review confirms the fix addresses the root cause

## Next Steps

1. ✅ Fix applied to `src/degenbot/cli/aave.py`
2. Run the Aave update to verify the fix resolves the issue at block 21223796
3. Monitor for any regressions in other transactions
