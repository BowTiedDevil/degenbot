# Issue 0023: Collateral Transfer Burn Skipping Bug

**Date:** 2026-03-03

**Symptom:**
```
AssertionError: User 0x000000000000Bb1B11e5Ac8099E92e366B64c133: collateral balance (1) does not match scaled token contract (0) @ 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8 at block 20625560
```

## Root Cause

The `_process_collateral_transfer_with_match` function in `src/degenbot/cli/aave.py` was incorrectly skipping ERC20 transfer events when there was a matching SCALED_TOKEN_BURN event in the same transaction, regardless of whether the transfer was actually related to the burn.

In transaction `0x99ee400923ebf0a77c8797a70fb55cea151063cae6201a73cc243c16dd61232b` (block 20625560):
1. User supplies 1 wei WETH, receives 1 wei aEthWETH (log 104)
2. User transfers 1 wei aEthWETH to Repay Adapter (log 114)
3. Adapter burns 1 wei aEthWETH (log 120)
4. User supplies 1 wei WETH via adapter, receives 1 wei aEthWETH (log 135)
5. User burns 1 wei aEthWETH (log 154)

Net result should be 0, but the database showed 1 because:
- The transfer at log 114 (user -> adapter) was being skipped
- The code found a SCALED_TOKEN_BURN at log 154 with the same from_address and amount
- It incorrectly assumed the transfer would be burned and skipped processing it
- The adapter never received the +1 wei, but the user still had +1 from log 135

The fix adds a check to only skip transfers when the target is the zero address (direct burn), not when transferring to intermediate contracts like adapters.

## Transaction Details

- **Hash:** 0x99ee400923ebf0a77c8797a70fb55cea151063cae6201a73cc243c16dd61232b
- **Block:** 20625560
- **Type:** Repay With Collateral (morph() function)
- **User:** 0x000000000000Bb1B11e5Ac8099E92e366B64c133
- **Asset:** aEthWETH (0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8)

## Fix

**File:** `src/degenbot/cli/aave.py`

**Location:** `_process_collateral_transfer_with_match` function (around line 3307-3338)

**Changes:**
Modified the burn-matching logic to only skip transfers when the target address is the zero address:

```python
# Only skip if the transfer target is the zero address (direct burn)
# or the Pool (burn via Pool). Don't skip transfers to adapters
# or other intermediate contracts that hold the tokens.
if scaled_event.target_address == ZERO_ADDRESS:
    # Skip this transfer as the burn will handle the balance reduction
    return
```

**Additional Fix:**
Added processing for `transfer_events` in `_process_operation` to handle ERC20 Transfer events to zero address that are attached to WITHDRAW operations:

```python
# Process transfer_events that represent burns (ERC20 Transfer to zero address)
# These are added to operations like WITHDRAW but need to be processed to update balances
if operation.operation_type in {
    OperationType.WITHDRAW,
    OperationType.REPAY,
    OperationType.REPAY_WITH_ATOKENS,
}:
    for transfer_event in operation.transfer_events:
        _process_transfer_event_burn(...)
```

## Key Insight

When processing transfers, we must distinguish between:
1. **Direct burns**: Transfer to zero address - these represent the actual burn and should be processed
2. **Transfers to intermediaries**: Transfer to adapters, pools, etc. - these must be processed to update both sender and recipient balances

The original code assumed any transfer with a matching burn event should be skipped, but this is only true for direct burns, not for transfers to intermediate contracts that may later burn the tokens.

## Refactoring

Consider adding a more robust transfer classification system that:
1. Tracks the lifecycle of aTokens through a transaction
2. Distinguishes between transient transfers (to adapters) and final transfers (burns)
3. Ensures all balance changes are accounted for regardless of intermediate hops

## Testing

The fix was validated by:
1. Running the update command to block 20625560 - now passes successfully
2. Running existing test suite - 3 pre-existing failures in unrelated GHO tests (confirmed by testing without changes)
3. Transfer to zero address handler tests pass
