# Aave Update Failure Report

**Issue:** BalanceTransfer Not Processed Before Burn in Same Transaction

**Date:** 2026-03-06

**Issue ID:** 0030

---

## Symptom

```
AssertionError: User 0x1CEBd13797636b94C5CaD7108CEbE42D2Fce7732: collateral balance (-70825098) does not match scaled token contract (0) @ 0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c at block 21223796
```

---

## Root Cause

The `_process_collateral_transfer_with_match` function in `src/degenbot/cli/aave.py` contains logic (lines 3456-3483) that skips updating the recipient's balance when it detects that the recipient will burn the tokens later in the same transaction. This optimization is intended to handle cases where a contract receives and immediately burns aTokens as part of an atomic operation.

However, this skip logic is incorrectly applied to **BalanceTransfer** events that are processed as standalone **BALANCE_TRANSFER** operations. When a BalanceTransfer event provides aTokens to a user, and that user subsequently burns aTokens in the same transaction (e.g., for `repayWithATokens`), the BalanceTransfer amount is never added to the user's balance, but the burn amount is still subtracted, resulting in a negative balance.

In this specific transaction:
1. **BalanceTransfer** event adds 70,825,098 aEthUSDC to user 0x1CEBd... (from Angle Protocol Distributor)
2. **Burn** event removes 77,666,633 aEthUSDC from the same user (for debt repayment)
3. The BalanceTransfer is skipped due to the burn detection logic
4. The Burn is processed, subtracting from a balance of 0 (starting balance was 6,841,535)
5. Result: -70,825,098 instead of 0

---

## Transaction Details

- **Hash:** `0x59da72746a5e34b7f93502047d4e11104f447f419563bb2ef3131ae8456e0522`
- **Block:** 21223796 (Nov-19-2024 07:13:11 UTC)
- **Type:** Gnosis Safe multicall with Aave V3 operations
- **User:** `0x1CEBd13797636b94C5CaD7108CEbE42D2Fce7732` (bergspyder.eth Safe)
- **Asset:** aEthUSDC (`0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c`)

### Operations Sequence
1. **Angle Protocol Distributor.claim()** - Claims ANGLE rewards, mints aEthUSDC to distributor
2. **BalanceTransfer** event - Transfers 70,825,098 scaled aEthUSDC from distributor to user (Log Index 496)
3. **Aave Pool.repayWithATokens()** - Repays debt using aTokens
   - Transfer event: 77,666,633 aEthUSDC from user to null (Log Index 501)
   - Burn event: 77,666,633 aEthUSDC burned (Log Index 502)

### Key Events
- **Event 5 (Log 496):** BalanceTransfer from `0x3ef3d8ba...` to `0x1CEBd...` - **70,825,098 aEthUSDC**
- **Event 10 (Log 501):** Transfer from `0x1CEBd...` to `0x0000...` (burn) - 77,666,633 aEthUSDC
- **Event 11 (Log 502):** Burn event - 77,666,633 aEthUSDC

---

## Key Insight

**BalanceTransfer events must always be processed.** Unlike ERC20 Transfer events that may be followed by immediate burns as part of atomic operations, BalanceTransfer events represent the actual movement of scaled balances between addresses. Skipping them causes accounting errors because:

1. The sender's balance is correctly decremented
2. The recipient's balance is NOT incremented (due to skip logic)
3. Subsequent burns decrement from the wrong starting balance

The fundamental premise holds: **database values are accurate**. The failure occurred because the processing code incorrectly skipped a valid BalanceTransfer that should have been applied.

---

## Actual Root Cause

The issue was **threefold**:

### 1. Standalone BalanceTransfer Events Not Assigned to Operations
**Location:** `src/degenbot/cli/aave_transaction_operations.py` - `_create_transfer_operations`

The function only processed ERC20 Transfer events (`ev.index is None`) and looked for paired BalanceTransfer events. **Standalone BalanceTransfer events (`index > 0`) that had no paired ERC20 Transfer were never assigned to any operation** and were never processed. The Angle Protocol reward distribution used exactly this pattern - a standalone BalanceTransfer to the user.

### 2. Burn Detection Applied to Paired ERC20 Transfers
**Location:** `src/degenbot/cli/aave.py` - `_process_collateral_transfer_with_match` (lines ~3486-3508)

When an ERC20 Transfer had a paired BalanceTransfer (as part of a BALANCE_TRANSFER operation), the burn detection logic was still applied to the ERC20 Transfer. This caused the recipient update to be skipped even though the BalanceTransfer amount (not the ERC20 amount) should have been used.

### 3. Burn Incorrectly Matched Any Preceding BalanceTransfer
**Location:** `src/degenbot/cli/aave.py` - `_process_collateral_burn_with_match` (lines ~2836-2866)

The burn processing logic searched for ANY preceding BalanceTransfer event TO the user and used its amount. This logic was designed for WITHDRAW operations where the BalanceTransfer is part of the same atomic operation, but it was being applied to ALL burns including REPAY_WITH_ATOKENS. The user's burn at log 502 incorrectly matched the reward BalanceTransfer at log 496.

---

## Applied Fix

### Fix 1: Create Operations for Standalone BalanceTransfer Events
**Location:** `src/degenbot/cli/aave_transaction_operations.py` - Added after line 1589

Added a second loop to create BALANCE_TRANSFER operations for any unassigned BalanceTransfer events:

```python
# Process standalone BalanceTransfer events (no paired ERC20 Transfer)
# These can occur when rewards are distributed directly via BalanceTransfer
# ref: Issue #0030 - Standalone BalanceTransfer events must be processed
for ev in scaled_events:
    # Skip already assigned events
    if ev.event["logIndex"] in assigned_indices or ev.event["logIndex"] in local_assigned:
        continue

    # Only process BalanceTransfer events (index > 0 indicates BalanceTransfer)
    if ev.index is None or ev.index == 0:
        continue

    # Only process transfer event types
    if ev.event_type not in {
        "COLLATERAL_TRANSFER",
        "DEBT_TRANSFER",
        "GHO_DEBT_TRANSFER",
    }:
        continue

    # Create BALANCE_TRANSFER operation for standalone BalanceTransfer
    operations.append(
        Operation(
            operation_id=operation_id,
            operation_type=OperationType.BALANCE_TRANSFER,
            pool_event=None,
            scaled_token_events=[ev],
            transfer_events=[],
            balance_transfer_events=[],
        )
    )
    local_assigned.add(ev.event["logIndex"])
    operation_id += 1
```

### Fix 2: Skip Burn Detection for ERC20 Transfers with Paired BalanceTransfer
**Location:** `src/degenbot/cli/aave.py` - `_process_collateral_transfer_with_match` (lines ~3486-3495)

Added check to prevent burn detection when the ERC20 Transfer has a paired BalanceTransfer:

```python
# Check if the recipient immediately burns the tokens (without a WITHDRAW operation)
# Only apply this skip logic to ERC20 Transfers (index is None or 0), NOT to
# BalanceTransfer events (index > 0). BalanceTransfer events represent the actual
# movement of scaled balances and must always be processed.
# Also, don't skip if this ERC20 Transfer has a paired BalanceTransfer event,
# as the BalanceTransfer represents the actual balance movement.
# ref: Issue #0026 - Don't skip if the burn is part of a WITHDRAW operation
# ref: Issue #0030 - BalanceTransfer events must always update recipient balance
skip_recipient_update = False
has_paired_balance_transfer = (
    operation is not None
    and operation.balance_transfer_events
    and any(
        bt_event["logIndex"] > scaled_event.event["logIndex"]
        for bt_event in operation.balance_transfer_events
    )
)
if tx_context is not None and (scaled_event.index is None or scaled_event.index == 0) and not has_paired_balance_transfer:
    # ... rest of existing burn detection logic ...
```

### Fix 3: Only Match BalanceTransfer for WITHDRAW Operations
**Location:** `src/degenbot/cli/aave.py` - `_process_collateral_burn_with_match` (line ~2836)

Changed the BalanceTransfer matching to only apply for WITHDRAW operations:

```python
# If no tracked transfer found and this is a WITHDRAW operation,
# search through events for a paired BalanceTransfer.
# Only apply BalanceTransfer matching for WITHDRAW operations where the
# BalanceTransfer is part of the same atomic operation.
# ref: Issue #0030 - Don't match BalanceTransfer for non-WITHDRAW burns
if scaled_amount is None and operation is not None and operation.operation_type == OperationType.WITHDRAW:
    # ... rest of BalanceTransfer search logic ...
```

---

## Verification

After applying all three fixes, the update successfully processed block 21223796:

```
$ uv run degenbot aave update --to-block 21223796
Updating Aave Ethereum Market (chain 1): block range 21,223,796 - 21,223,796
Market 1 (chain 1) successfully updated to block 21,223,796
```

The user's collateral balance correctly reflects:
1. **BalanceTransfer from distributor:** +70,825,098 aEthUSDC received
2. **Burn for repayWithATokens:** -77,666,633 aEthUSDC burned  
3. **Final balance:** 0 (correct, as the starting balance of 6,841,535 + 70,825,098 - 77,666,633 ≈ 0 with rounding)

---

## Refactoring Recommendations

**Improve BalanceTransfer Processing Logic**

The current implementation conflates ERC20 Transfer events and BalanceTransfer events in the `_process_collateral_transfer_with_match` function. A cleaner approach would be:

1. **Separate handlers:** Create distinct processing functions for ERC20 Transfers vs BalanceTransfer events
2. **Clearer semantics:** BalanceTransfer events (index > 0) should always update balances atomically
3. **Documentation:** Add explicit comments explaining why BalanceTransfer events must be processed even when followed by burns
4. **Validation:** Add assertions to ensure BalanceTransfer events always result in balance updates

This refactoring would prevent similar bugs where the optimization logic for one event type incorrectly affects another event type with different semantics.
