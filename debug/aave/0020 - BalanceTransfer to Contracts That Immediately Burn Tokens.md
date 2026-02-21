# Aave Debug Progress

## Issue: BalanceTransfer to Contracts That Immediately Burn Tokens Incorrectly Adds to Recipient Balance

**Date:** 2025-02-21

**Symptom:**
```
AssertionError: User 0xE4217040c894e8873EE19d675b6d0EeC992c2c0D: collateral balance (1000000000000000) does not match scaled token contract (0) @ 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8 at block 16496928
```

**Root Cause:**
The `_process_scaled_token_balance_transfer_event()` function in `src/degenbot/cli/aave.py` incorrectly added the transferred amount to the TO user's (recipient's) balance when the recipient was a contract that immediately burned the received aTokens (e.g., ParaSwap adapter). In this swap pattern:

1. User A transfers aTokens to ParaSwap adapter (BalanceTransfer event)
2. Adapter immediately burns aTokens (Burn event)
3. Adapter swaps underlying tokens and supplies new asset on behalf of User A

The BalanceTransfer represents an actual transfer of tokens from User A's position. The FROM user's balance SHOULD be reduced because the tokens actually leave their position. The TO user's balance should NOT be increased because they immediately burn the tokens and don't actually hold them.

The previous fix for issue #0018 (Gateway contracts) incorrectly set `skip_from_user_balance_update = True`, which prevented the FROM user's balance from being reduced. While that was correct for Gateway contracts where tokens are pulled but remain under user's control, it's incorrect for swap adapters where tokens actually leave the user's position.

**Transaction Details:**
- **Hash:** 0x4a88a8c6a43b5df2ee59ebcf266225fbc5b876f202009422f0f9d05cc4915f35
- **Block:** 16496928
- **Type:** Multi-hop swap via ParaSwap adapter using aTokens
- **User (sender):** 0xE4217040c894e8873EE19d675b6d0EeC992c2c0D
- **Recipient (adapter):** 0x872fBcb1B582e8Cd0D0DD4327fBFa0B4C2730995 (ParaSwap adapter contract)
- **Asset:** aEthWETH (0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8)
- **Amount:** 1000000000000000 (0.001 WETH)

**Event Sequence:**
1. **BalanceTransfer** (logIndex 107): aWETH from `0xE421...` → `0x872f...` (ParaSwap adapter) for 0.001 aWETH
2. **Burn** (logIndex 110): `0x872f...` burns the aWETH immediately
3. **Withdraw** (logIndex 113): WETH withdrawn from Aave Pool by adapter
4. **Swap**: WETH → DAI via ParaSwap
5. **Supply + Mint** (logIndex 124-126): DAI supplied to Aave, aDAI minted to `0xE421...`

**Contract Balances (Verified via cast calls):**

At block 16496927 (before):
- User `0xE421...` aWETH balance: **1000000000000000**
- Adapter `0x872f...` aWETH balance: **0**

At block 16496928 (after):
- User `0xE421...` aWETH balance: **0** (tokens were transferred and burned)
- Adapter `0x872f...` aWETH balance: **0** (burned immediately)

**Why the Verification Failed:**

The database showed user `0xE421...` has balance **1000000000000000**, but the contract showed **0**:

1. The `skip_from_user_balance_update = True` from issue #0018 prevented the FROM balance from being reduced
2. This was incorrect for swap adapters where tokens actually leave the user's position
3. The FROM balance should be reduced because the tokens are transferred away and burned

**Fix:**

In `_process_scaled_token_balance_transfer_event()`, when the recipient immediately burns the tokens:
1. The FROM user's balance SHOULD be reduced (tokens actually leave their position)
2. The TO user's balance should NOT be increased (recipient burns immediately)
3. The subsequent Burn event should be skipped to avoid negative balances

**Code Changes in `src/degenbot/cli/aave.py`:**

1. Added `skipped_burn_events` tracking to TransactionContext (lines 145-150):
```python
# Track Burn events that should be skipped because they correspond to the immediate
# burn pattern from a BalanceTransfer (e.g., ParaSwap adapter receiving and immediately
# burning aTokens). Key: Burn event logIndex, Value: True if should be skipped.
# This prevents negative balances when the recipient never actually held the tokens.
# ref: Bug #0020
skipped_burn_events: dict[int, bool] = field(default_factory=dict)
```

2. Modified `_process_scaled_token_burn_event()` to check for skipped events (lines 3892-3902):
```python
# Skip burn events that were marked for skipping by BalanceTransfer processing
# (e.g., when a contract receives aTokens via BalanceTransfer and immediately burns them)
# ref: Bug #0020
if context.tx_context is not None and context.tx_context.skipped_burn_events.get(
    context.event["logIndex"], False
):
    return
```

3. Updated BalanceTransfer processing to only skip TO user update (lines 4103-4135):
```python
# Check if this is a BalanceTransfer to a contract that immediately burns the tokens
# (e.g., ParaSwap adapters). In this case, the tokens actually leave the FROM user's
# position and are burned by the recipient. The FROM user's balance SHOULD be reduced
# because the tokens are transferred away. The TO user's balance should NOT be increased
# because they immediately burn the tokens and don't actually hold them.
# ref: TX 0x4a88a8c6a43b5df2ee59ebcf266225fbc5b876f202009422f0f9d05cc4915f35
skip_from_user_balance_update = False
if context.tx_context is not None:
    # Only check the next few events for immediate burns, not all subsequent events
    # This prevents incorrectly skipping balance updates when the recipient burns
    # their own tokens later in the transaction (e.g., for withdrawal).
    subsequent_events = context.tx_context.get_subsequent_events(context.event)
    # Check only up to 5 subsequent events for immediate burn
    for subsequent_event in subsequent_events[:5]:
        subsequent_topic = subsequent_event["topics"][0]
        if (
            subsequent_topic == AaveV3Event.SCALED_TOKEN_BURN.value
            and _decode_address(subsequent_event["topics"][1]) == to_address
        ):
            # The recipient burns the tokens immediately after receiving them
            # Only skip if the burn amount equals the transfer amount (indicating
            # immediate burn of received tokens, not a withdrawal of own tokens)
            # Burn event data: (value, balanceIncrease, index)
            burn_value, _, _ = _decode_uint_values(event=subsequent_event, num_values=3)
            if burn_value == event_amount:
                # The FROM user's balance SHOULD be reduced because the tokens actually
                # leave their position (the recipient burns them). Only skip the TO
                # user's balance update since they don't actually hold the tokens.
                # ref: Bug #0020
                skip_to_user_balance_update = True
                # Mark the burn event to be skipped so it doesn't process later
                # and cause a negative balance
                context.tx_context.skipped_burn_events[subsequent_event["logIndex"]] = True
                break
```

**Key Insight:**

BalanceTransfer events have different semantics depending on the recipient:

1. **User-to-User transfers:** BalanceTransfer represents actual scaled balance movement. Both FROM (reduced) and TO (increased) balances should be updated.

2. **User-to-Adapter transfers (swap patterns):** BalanceTransfer represents actual token movement where the adapter burns tokens immediately. The FROM balance SHOULD be reduced (tokens leave), but the TO balance should NOT be increased (recipient burns immediately).

3. **User-to-Gateway transfers (withdrawETH patterns):** BalanceTransfer represents internal accounting where the Gateway holds tokens temporarily before returning them. Neither FROM nor TO balances should be updated (handled in issue #0018).

The critical distinction between cases 2 and 3 is whether the tokens actually leave the user's position permanently (case 2: swap) or are temporarily held and then returned/unwrapped (case 3: Gateway withdraw).

**Testing:**

Created comprehensive test suite in `tests/cli/test_aave_balance_transfer_immediate_burn.py`:
- `test_balance_transfer_with_immediate_burn_updates_from_not_to`: Verifies FROM balance is reduced but TO balance is not increased
- `test_balance_transfer_without_burn_updates_both_balances`: Verifies normal transfers update both balances
- `test_balance_transfer_with_different_burn_amount`: Verifies transfers proceed normally when burn amount differs

All tests pass with the fix.

**Verification:**

- Aave update now processes block 16496928 successfully
- Verification passes for user 0xE4217040c894e8873EE19d675b6d0EeC992c2c0D
- No negative balance assertions

**Refactoring:**

Consider creating a more robust pattern detection system that distinguishes between:
1. Swap adapters (tokens leave user's position permanently)
2. Gateway contracts (tokens temporarily held then returned/unwrapped)
3. Regular user transfers (actual balance movement)

This could be done by:
1. Maintaining a registry of known contract types (swappers vs gateways)
2. Analyzing the full transaction flow to determine if tokens are returned
3. Looking for specific function signatures (e.g., `swap()`, `withdrawETH()`)

This would be more explicit and less prone to false positives/negatives than the current event-pattern-based detection.
