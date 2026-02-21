# Aave Debug Progress

## Issue: BalanceTransfer to Gateway Contracts Incorrectly Reduces User Balance

**Date:** 2025-02-21

**Symptom:**
```
AssertionError: User 0x44c9788CdFAbE3cb15a3eEb6E63cd2ec709c8bbE: collateral balance (54441190707820206778) does not match scaled token contract (78238723943805300364) @ 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8 at block 23088578
```

**Root Cause:**
The `_process_scaled_token_balance_transfer_event()` function in `src/degenbot/cli/aave.py` incorrectly processed BalanceTransfer events when the recipient is a Gateway contract (e.g., WrappedTokenGatewayV3) that immediately burns the received aTokens. In this pattern:

1. User transfers aTokens to Gateway (ERC20 Transfer + BalanceTransfer event)
2. Gateway burns aTokens (Burn event)

The BalanceTransfer event represents internal accounting when the Gateway receives aTokens before burning them. The FROM user's balance should NOT be reduced because the tokens are being pulled to a contract that doesn't actually hold them (they're immediately burned).

**Transaction Details:**
- **Hash:** 0x9418aa15808900527e4d558c8260d344fb04783850b790c3cc53b721ef8368fe
- **Block:** 23088578
- **Type:** withdrawETH via WrappedTokenGatewayV3
- **User:** 0x44c9788CdFAbE3cb15a3eEb6E63cd2ec709c8bbE
- **Gateway:** 0xd01607c3C5eCABa394D8be377a08590149325722 (WrappedTokenGatewayV3)
- **Asset:** aEthWETH (0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8)
- **Events:**
  - Log 321: Transfer 25 WETH from user to Gateway
  - Log 322: BalanceTransfer ~23.8 scaled from user to Gateway
  - Log 327: Burn 25 WETH from Gateway

**Fix:**
Added logic to detect when a BalanceTransfer recipient immediately burns the tokens by checking for a subsequent Burn event from the same recipient. When this pattern is detected, skip the balance update for the FROM user.

**Code Changes in `src/degenbot/cli/aave.py`:**

1. Added `get_subsequent_events()` method to TransactionContext class (lines 155-157):
```python
def get_subsequent_events(self, event: LogReceipt) -> list[LogReceipt]:
    """Get all events in this transaction that occurred after the given event."""
    return [e for e in self.events if e["logIndex"] > event["logIndex"]]
```

2. Modified `_process_scaled_token_balance_transfer_event()` (lines 4079-4095):
```python
# Check if this is a BalanceTransfer to a contract that immediately burns the tokens
# (e.g., WrappedTokenGatewayV3). In this case, the BalanceTransfer represents internal
# accounting when the contract receives aTokens before burning them. The FROM user's
# balance should NOT be reduced because the tokens are being pulled to a contract
# that doesn't actually hold them (they're immediately burned).
# ref: TX 0x9418aa15808900527e4d558c8260d344fb04783850b790c3cc53b721ef8368fe
skip_from_user_balance_update = False
if context.tx_context is not None:
    for subsequent_event in context.tx_context.get_subsequent_events(context.event):
        subsequent_topic = subsequent_event["topics"][0]
        if (
            subsequent_topic == AaveV3Event.SCALED_TOKEN_BURN.value
            and _decode_address(subsequent_event["topics"][1]) == to_address
        ):
            # The recipient burns the tokens immediately after receiving them
            # Skip the balance update for the FROM user
            skip_from_user_balance_update = True
            break
```

3. Updated balance update logic to respect the skip flag (lines 4102-4108):
```python
from_user_starting_amount = from_user_position.balance
if not skip_from_user_balance_update:
    from_user_position.balance -= event_amount
```

**Key Insight:**
BalanceTransfer events have different semantics depending on the recipient:
1. **User-to-User transfers:** BalanceTransfer represents actual scaled balance movement. Both FROM and TO balances should be updated.
2. **User-to-Gateway transfers:** BalanceTransfer represents internal accounting for contracts that immediately burn tokens. The FROM user's balance should NOT be reduced because the tokens never actually leave the user's control in a way that affects their scaled position.

**Testing:**
- Aave update now processes block 23088578 successfully
- Verification passes for user 0x44c9788CdFAbE3cb15a3eEb6E63cd2ec709c8bbE
- No negative balance assertions

**Refactoring:**
Consider creating a more robust pattern detection system for Gateway contracts. Instead of checking for subsequent Burn events, we could:
1. Maintain a registry of known Gateway contract addresses
2. Check if the recipient is a contract that implements the Gateway interface
3. Look for specific function signatures (e.g., withdrawETH, swapAndDeposit) in the transaction

This would be more explicit and less prone to false positives/negatives than the current event-pattern-based detection.
