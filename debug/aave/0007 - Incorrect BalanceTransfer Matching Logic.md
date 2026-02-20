# Aave Debug Progress

## Issue: Incorrect BalanceTransfer Matching Logic

**Date:** 2025-02-19

**Symptom:**
```
AssertionError: User 0x5b5a05804B043Aaf9Edd3c75A68e4cF2A72641F9: debt balance (0) does not match scaled token contract (21609343359601319070) @ 0x72E95b8931767C79bA4EeE721354d6E99a61D004 at block 21711842
```

**Root Cause:**
The `_process_scaled_token_mint_event()` function had logic to skip processing BalanceTransfer events when they immediately followed a Mint event with the same value. This was intended to handle cases where a Mint (interest accrual) is followed by a BalanceTransfer (liquidator receiving collateral), but it incorrectly skipped legitimate BalanceTransfers to different recipients.

**Transaction Flow (0xcaf2f4c938415c22b2cbd5a6444ec566e3c4c6a0e00f85a2f3ac43b1aee33399):**
1. logIndex 68: Mint event for User A (interest accrual)
2. logIndex 70: Transfer event (liquidation collateral)
3. logIndex 72: Mint event for User B (interest accrual)
4. logIndex 73: BalanceTransfer from User B to User C (collateral transfer)
5. logIndex 76: Transfer event (debt collateral)

The BalanceTransfer at logIndex 73 was skipped because it immediately followed a Mint at logIndex 72, even though they had different recipients.

**Bug Location:** `_process_scaled_token_mint_event()` in `src/degenbot/cli/aave.py` lines 3465-3475

The problematic logic:
```python
# Check if this mint is immediately followed by a BalanceTransfer of the same value
# This indicates a liquidation where the mint is just interest accrual and the
# actual collateral transfer happens via BalanceTransfer
if (
    next_event is not None
    and next_event["topics"][0] == SCALED_TOKEN_BALANCE_TRANSFER
    and next_event["topics"][1] == event["topics"][1]  # Same 'from' address
    and _decode_single_uint_value(next_event["data"]) == event_amount
):
    # Skip this mint - the balance update will happen via BalanceTransfer
    return
```

The condition `next_event["topics"][1] == event["topics"][1]` checks if the Mint and BalanceTransfer have the same 'from' address, but it doesn't account for cases where the Mint and BalanceTransfer have different recipients (e.g., interest minted to User A, then BalanceTransfer from User A to User B).

### Fix
Modified the skip logic in `_process_scaled_token_mint_event()` to only skip when the Mint and BalanceTransfer have:
1. The same `from` address (both from the same user)
2. The same `to` address (both to the same recipient)
3. The same value

This ensures we only skip when it's truly a duplicate operation, not when it's a legitimate transfer to a different recipient.

**Code Changes:**
```python
# Check if this mint is immediately followed by a BalanceTransfer of the same value
# This indicates a liquidation where the mint is just interest accrual and the
# actual collateral transfer happens via BalanceTransfer
if (
    next_event is not None
    and next_event["topics"][0] == SCALED_TOKEN_BALANCE_TRANSFER
    and next_event["topics"][1] == event["topics"][1]  # Same 'from' address
    and next_event["topics"][2] == event["topics"][2]  # Same 'to' address (ADDED)
    and _decode_single_uint_value(next_event["data"]) == event_amount
):
    # Skip this mint - the balance update will happen via BalanceTransfer
    return
```

**Files Modified:**
- `src/degenbot/cli/aave.py`

### Transaction Details
- **Hash:** 0xcaf2f4c938415c22b2cbd5a6444ec566e3c4c6a0e00f85a2f3ac43b1aee33399
- **Block:** 21711842
- **Type:** Liquidation
- **User:** 0x5b5a05804B043Aaf9Edd3c75A68e4cF2A72641F9
- **Asset:** aUSDC (0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c)
- **Events:**
  - Mint (log 0x44): user=0x5b5a0580..., amount=21609343359601319070
  - Transfer (log 0x45): collateral transfer
  - Mint (log 0x46): user=0x5b5a0580..., amount=158210497782542
  - BalanceTransfer (log 0x47): from=0x5b5a0580..., to=0x5b5a0580..., value=158210497782542
  - Transfer (log 0x4c): debt transfer
  - BalanceTransfer (log 0x173): from=0x5b5a0580..., to=0x0f4a1d7f..., value=2
  - BalanceTransfer (log 0x178): from=0x0f4a1d7f..., to=0xccd58333..., value=2

**Key Insight:**
The BalanceTransfer matching logic was designed to prevent double-counting when a Mint event (pure interest accrual) is immediately followed by a BalanceTransfer of the same value. However, it failed to account for scenarios where:
1. A Mint mints interest to User A
2. User A transfers those tokens to User B via BalanceTransfer
3. The Mint and BalanceTransfer have the same value but different recipients

In such cases, the BalanceTransfer to User B should NOT skip the balance update - User B legitimately receives tokens that were minted to User A.

**Testing:**
- Aave update now processes block 21711842 successfully
- No negative balance assertions

**Refactoring:**
Consider extracting the Mint-to-BalanceTransfer matching logic into a helper function with clear documentation about when matching should and should not occur. The current inline matching logic is complex and easy to get wrong.
