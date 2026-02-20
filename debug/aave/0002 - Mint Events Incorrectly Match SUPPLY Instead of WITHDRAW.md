# Aave Debug Progress

## Issue: Mint Events Incorrectly Match SUPPLY Instead of WITHDRAW

**Date:** 2025-02-18

**Symptom:** 
```
AssertionError: User 0x72e518ba868153A0a4D0cE275931bd4be3A7ddCd: collateral balance (9390824466508831968) does not match scaled token contract (9384054295024955782) @ 0x32a6268f9Ba3642Dda7892aDd74f1D34469A4259 at block 21272539
```

**Root Cause:** 
In `_process_collateral_mint_event()` in `src/degenbot/cli/aave.py`, the pool event matching logic always tried SUPPLY events first, regardless of whether the Mint event represented a deposit or withdrawal. When `balance_increase > value` (indicating interest accrual during a withdrawal), the code would incorrectly match a SUPPLY event and use its amount to calculate `scaled_delta`, instead of matching a WITHDRAW event or letting the processor calculate the delta from the Mint event itself.

The matching order was:
1. SUPPLY (for user.address)
2. WITHDRAW (for caller_address)
3. REPAY (for user.address)

When processing Mint(230) with `balanceIncrease=8,129,187,089,154,332` and `value=8,128,842,184,652,540` (where balanceIncrease > value), the code would find SUPPLY(232) for the same user and use its amount (344,904,501,792) to calculate `scaled_delta`, completely ignoring the actual interest accrual amount.

**Transaction Details:**
- **Hash:** 0x80598c1ea819a292515c55e93c6b5a9a2148baff577585927e414fdcde8eb9f6
- **Block:** 21272539
- **Type:** Withdrawal with interest accrual
- **User:** 0x72e518ba868153A0a4D0cE275931bd4be3A7ddCd
- **Asset:** aEthUSDS (0x32a6268f9Ba3642Dda7892aDd74f1D34469A4259)
- **Events:** 
  - Mint at logIndex 230: value=8,128,842,184,652,540, balanceIncrease=8,129,187,089,154,332 (balanceIncrease > value)
  - SUPPLY at logIndex 232: amount=344,904,501,792 (wrongly matched)

**Fix:**
Updated `_process_collateral_mint_event()` in `src/degenbot/cli/aave.py` to choose the matching sequence based on the relationship between `event_amount` and `balance_increase`:

```python
# Determine which event type to match based on value vs balance_increase
if event_amount > balance_increase:
    # Standard deposit - look for SUPPLY first
    match_sequence = [
        (AaveV3Event.SUPPLY.value, user.address),
        (AaveV3Event.WITHDRAW.value, caller_address),
        (AaveV3Event.REPAY.value, user.address),
    ]
else:
    # balance_increase > value - interest accrual during withdraw
    # Look for WITHDRAW first since this is a withdrawal operation
    match_sequence = [
        (AaveV3Event.WITHDRAW.value, caller_address),
        (AaveV3Event.SUPPLY.value, user.address),
        (AaveV3Event.REPAY.value, user.address),
    ]
```

**Key Insight:** 
Mint events can be emitted in three scenarios based on the relationship between `value` and `balanceIncrease`:
1. **`value > balance_increase`**: SUPPLY - user deposit, new tokens minted
2. **`balance_increase > value`**: WITHDRAW - interest accrual before withdrawal
3. **`value == balance_increase`**: Interest accrual without user action (skip matching)

The matching logic must consider this relationship to find the correct pool event.

**Refactoring:**
Consider extracting the pool event matching logic into a separate helper function that takes the match sequence as a parameter, making the code more testable and the intent clearer.
