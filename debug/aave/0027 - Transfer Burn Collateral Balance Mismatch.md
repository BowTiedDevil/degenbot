# Issue #0027: Transfer Burn Collateral Balance Mismatch

**Date:** 2026-03-05

## Symptom

AssertionError: User 0x872fBcb1B582e8Cd0D0DD4327fBFa0B4C2730995: collateral balance (-1000000000000000) does not match scaled token contract (0) @ 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8 at block 16496928

## Root Cause

The ERC20 Transfer and BalanceTransfer event pairing logic was broken due to an incorrect index check. The code was checking `if ev.index == 0:` to identify ERC20 Transfer events, but ERC20 Transfer events actually have `index=None`, not `index=0`.

This caused two issues:

1. **Event pairing failed**: The logic to pair ERC20 Transfer events with BalanceTransfer events never executed, because `ev.index == 0` was always False for ERC20 Transfer events (they have `index=None`).

2. **Double counting**: Without proper pairing, both the ERC20 Transfer and BalanceTransfer events were processed as separate transfers, doubling the recipient's balance.

3. **Incorrect burn amount**: The burn used the BalanceTransfer amount from the event matching logic, but the transfer had added the ERC20 Transfer amount (which may differ slightly due to interest accrual).

## Transaction Details

- **Hash:** 0x4a88a8c6a43b5df2ee59ebcf266225fbc5b876f202009422f0f9d05cc4915f35
- **Block:** 16496928
- **Type:** Withdraw (via ParaSwap router)
- **User:** 0x872fBcb1B582e8Cd0D0DD4327fBFa0B4C2730995
- **Asset:** WETH (aWETH)

### Event Sequence

1. **Log 104 (0x68):** ERC20 Transfer from 0xE421... to 0x872f... for 0.001 WETH
2. **Log 107 (0x6b):** BalanceTransfer from 0xE421... to 0x872f... for 0.001 scaled
3. **Log 111 (0x6f):** Burn from 0x872f... for 0.001 scaled

### Expected Balance Flow

- Start: 0
- Transfer (log 104): +0.001
- BalanceTransfer (log 107): Skip (paired with log 104)
- Burn (log 111): -0.001
- End: 0

### Actual Balance Flow (Before Fix)

- Start: 0
- Transfer (log 104): +0.001
- BalanceTransfer (log 107): +0.001 (not skipped - double counting!)
- Burn (log 111): -0.001
- End: 0.001 (should be 0)

## Fix

### File: `src/degenbot/cli/aave_transaction_operations.py`

**Line 1517:** Changed `is_erc20_transfer = ev.index is 0` to `is_erc20_transfer = ev.index is None`

**Line 1533:** Changed `if bt_ev.index == 0:` to `if bt_ev.index is None:`

**Line 1554:** Changed `if bt_ev.index == 0:` to `if bt_ev.index is None:`

This ensures that:
1. ERC20 Transfer events (index=None) are correctly identified
2. BalanceTransfer events (index>0) are correctly identified and paired with their corresponding ERC20 Transfers

### File: `src/degenbot/cli/aave.py`

**Lines 2889-2905:** Modified the WITHDRAW check to only use raw_amount when scaled_amount is None. This ensures that when a BalanceTransfer is found, its exact amount is used for the burn, ensuring perfect cancellation.

**Lines 3476-3502:** Updated the immediate burn detection to check for WITHDRAW events anywhere in the transaction (not just after the transfer), preventing incorrect skipping of recipient updates during withdrawal operations.

## Key Insight

The distinction between ERC20 Transfer and BalanceTransfer events is critical for accurate balance tracking:

- **ERC20 Transfer:** Standard ERC20 event, no index field, amount includes accrued interest
- **BalanceTransfer:** Aave-specific event, includes index field, amount is the scaled balance

When these events occur together (during transfers between users), only the BalanceTransfer should update balances to avoid double-counting. The ERC20 Transfer provides the aToken amount (with interest), while the BalanceTransfer provides the exact scaled balance change.

## Refactoring

1. **Consolidate event detection:** The logic for identifying ERC20 Transfer vs BalanceTransfer is scattered across multiple files. Consider centralizing this in a single helper function.

2. **Type safety:** The `index` field in `ScaledTokenEvent` should use a more explicit type (e.g., `Optional[int]`) with clear documentation about when it's None vs a positive integer.

3. **Event pairing:** The current pairing logic in `_create_transfer_operations` is complex and error-prone. Consider using a more robust matching algorithm based on event topics and log indices.

4. **Documentation:** Add explicit comments explaining the difference between ERC20 Transfer and BalanceTransfer events, and why BalanceTransfer takes precedence for balance updates.

## Verification

After the fix, the update command successfully processes block 16496928:

```bash
$ uv run degenbot aave update --to-block=16496928
Updating Aave Ethereum Market (chain 1): block range 16,491,071 - 16,496,928
Market 1 (chain 1) successfully updated to block 16,496,928
```

The balance verification passes, confirming that the transfer and burn amounts now correctly cancel out.
