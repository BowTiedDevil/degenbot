# Issue: Deposit via Router with value == balanceIncrease

**Date:** 2025-02-21

**Symptom:**
```
AssertionError: User 0xf6Da0E829bC40414a4A2eF89f1C66C74CAC74BF2: collateral balance (285570438134518922) does not match scaled token contract (14278561440645144) @ 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8 at block 23088579
```

## Root Cause

This bug has two parts:

### Part 1: Mint Event Processing

When processing a Mint event where `value == balance_increase`, the collateral processors (v1, v4, v5) assumed this was "pure interest accrual" and set `balance_delta = 0`. This is correct when there's no SUPPLY event (e.g., interest accrual before transfer), but incorrect when the deposit comes through a router contract (like 1inch) that doesn't emit a SUPPLY event.

In this transaction, user 0xf6Da... deposited via 1inch router:
- The deposit happened to equal the accrued interest exactly
- This resulted in `value == balance_increase` in the Mint event
- The processor incorrectly treated this as pure interest (0 delta)

### Part 2: BalanceTransfer Skip Logic

The `skip_from_user_balance_update` logic in `_process_scaled_token_balance_transfer_event` was designed to handle WrappedTokenGatewayV3-style transactions where a contract receives aTokens and immediately burns them. However, it incorrectly skipped balance updates for legitimate transfers to contracts that later burn their own tokens (not the received ones).

The original logic:
- Checked if recipient burns tokens at any point after receiving them
- If yes, skipped the sender's balance update

This was wrong because user 0x5141... received tokens via BalanceTransfer, then later withdrew their own collateral (burning their own tokens, not the received ones).

## Transaction Details

- **Hash:** 0x3f9143ede0a37a540f0b2a2a3436087376d023e328330a91d1b3837cc5d14d85
- **Block:** 23088579
- **Type:** Deposit via 1inch router + BalanceTransfer
- **User:** 0xf6Da0E829bC40414a4A2eF89f1C66C74CAC74BF2
- **Asset:** aWETH (0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8)

### Event Sequence

| logIndex | Event | User | Amount |
|----------|-------|------|--------|
| 560 | Mint | 0xf6Da... | value=balanceIncrease (43,717,400,992) |
| 562 | BalanceTransfer | 0xf6Da... → 0x5141... | 271,291,876,693,873,778 |
| 565 | Mint | 0x39041... | value=balanceIncrease (2,500,000,000) |
| 567 | BalanceTransfer | 0x5141... → 0x39041... | 678,229,691,734,684 |
| 571 | Burn | 0x5141... (withdraw) | 270,613,647,002,139,094 |

## Fix

### File 1: `src/degenbot/aave/processors/collateral/v1.py` (and v4.py, v5.py)

Changed the Mint event processing logic to check if `scaled_amount` is provided:

```python
else:
    # value == balance_increase: deposit amount equals accrued interest, OR
    # pure interest accrual without a deposit (e.g., before transfer).
    # If scaled_amount is provided from a matched SUPPLY event, it's a deposit.
    # Otherwise, it's pure interest accrual where only the index updates.
    if event_data.scaled_amount is not None:
        # Deposit where deposit amount equals interest: use scaled amount from SUPPLY
        balance_delta = event_data.scaled_amount
    else:
        # Pure interest accrual - the user's scaled balance doesn't change
        balance_delta = 0
    is_repay = False
```

### File 2: `src/degenbot/cli/aave.py`

#### Change 1: Always attempt to match pool events

Changed from skipping matching when `event_amount == balance_increase` to always attempting to match:

```python
# Always attempt to match pool events, even when value == balance_increase.
# This handles the edge case where deposit amount equals accrued interest.
# See debug/aave/0019 for details.
if True:  # Always match, changed from: event_amount != balance_increase
```

#### Change 2: Fix BalanceTransfer skip logic

Updated the skip logic to check if the burn amount equals the transfer amount:

```python
for subsequent_event in subsequent_events[:5]:
    subsequent_topic = subsequent_event["topics"][0]
    if (
        subsequent_topic == AaveV3Event.SCALED_TOKEN_BURN.value
        and _decode_address(subsequent_event["topics"][1]) == to_address
    ):
        # The recipient burns the tokens immediately after receiving them
        # Only skip if the burn amount equals the transfer amount (indicating
        # immediate burn of received tokens, not a withdrawal of own tokens)
        burn_value, _, _ = _decode_uint_values(event=subsequent_event, num_values=3)
        if burn_value == event_amount:
            skip_from_user_balance_update = True
            break
```

## Key Insight

1. **Router deposits don't emit SUPPLY events:** When depositing via aggregators like 1inch, the deposit may not go through the standard Aave Pool.supply() function, so no SUPPLY event is emitted. The Mint event alone must be used to track the balance change.

2. **BalanceTransfer skip logic needs amount matching:** Simply checking if the recipient burns tokens is insufficient. We must verify that the burn amount equals the received amount to distinguish between:
   - Immediate burn of received tokens (skip sender update - internal accounting)
   - Recipient burning their own tokens later (don't skip - legitimate transfer)

## Refactoring Suggestions

1. **Add router contract detection:** Consider maintaining a list of known router contracts (1inch, ParaSwap, etc.) that may deposit without SUPPLY events, and handle them specially.

2. **Improve BalanceTransfer skip detection:** Instead of checking burn amount equality, consider checking if the recipient is a known gateway contract (like WrappedTokenGatewayV3) that has special behavior.

3. **Add transaction-level validation:** After processing all events in a transaction, verify that the sum of balance changes across all users equals zero (conservation of aTokens).

4. **Document event patterns:** Create a reference document showing common event patterns for different transaction types (direct supply, router deposit, liquidation, flash loan liquidation, etc.).
