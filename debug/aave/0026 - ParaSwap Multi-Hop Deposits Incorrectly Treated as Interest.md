# Issue: ParaSwap Multi-Hop Deposits Incorrectly Treated as Interest

**Date:** 2025-02-21

**Symptom:**
```
AssertionError: User 0xAf8Eb92B802503A4737F6fBa38B9D734cb22A28b: collateral balance (1232132206) does not match scaled token contract (1233256387) @ 0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c at block 18145516
```

## Root Cause

The Bug #0024 fix added a validation check that incorrectly treats valid ParaSwap multi-hop deposits as pure interest accrual when `calculated_scaled_amount != event_amount`.

### Transaction Analysis

**Transaction:** `0xc65753ab5751d591e08ac7a89910b129dbe3d09e9fcc94e32fc0a8d9a0da07a9`
**Block:** 18145516
**Type:** ParaSwap repay via Aave V3
**User:** `0xAf8Eb92B802503A4737F6fBa38B9D734cb22A28b`
**Asset:** aUSDC (`0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c`)

**Event Counts:**
- 17 SUPPLY events (logIndices: 17, 43, 78, 112, 147, 181, 215, 249, 283, 317, 351, 385, 419, 453, 487, 521, 555)
- 16 BalanceTransfer events (logIndices: 20, 55, 90, 124, 159, 193, 227, 261, 295, 329, 363, 397, 431, 465, 499, 533)
- 16 Mint events with `value == balance_increase` and `scaledAmount = 0` in event data

**The Problem:**

In `_process_collateral_mint_event()` (lines 3071-3076 of `aave.py`):

```python
if event_amount == balance_increase and calculated_scaled_amount != event_amount:
    # This is not a matching SUPPLY event - it's pure interest accrual
    matched_pool_event = None
    scaled_amount = None
else:
    scaled_amount = calculated_scaled_amount
```

When processing the 16 Mint events:
1. Each Mint matches a SUPPLY event via EventMatcher
2. The Bug #0024 check compares `calculated_scaled_amount` (from SUPPLY) to `event_amount` (from Mint)
3. In ParaSwap multi-hop transactions, these amounts differ significantly (SUPPLY raw amount vs Mint accrued value)
4. The check incorrectly sets `scaled_amount = None`
5. In the processor, `balance_delta = 0` when `scaled_amount is None` and `value == balance_increase`
6. Result: 16 deposits totaling ~1,124,181 units are not added to the balance

**Why This Happens:**

In a ParaSwap repay transaction with 16 cycles:
```
1. User's collateral accrues interest (value = balanceIncrease)
2. SUPPLY event is emitted for the deposit
3. Mint event is emitted with value = balanceIncrease (includes interest)
4. Transfer event sends aTokens to router
5. Burn event burns router's aTokens
6. BalanceTransfer event records the transfer
```

The Bug #0024 check was designed to prevent pure interest accrual Mints from incorrectly matching deposit SUPPLY events. However, it also catches valid multi-hop deposits where the SUPPLY amount differs from the Mint value due to the transaction structure.

## Transaction Details

- **Hash:** `0xc65753ab5751d591e08ac7a89910b129dbe3d09e9fcc94e32fc0a8d9a0da07a9`
- **Block:** 18145516
- **Chain:** Ethereum mainnet (chain_id: 1)
- **User:** `0xAf8Eb92B802503A4737F6fBa38B9D734cb22A28b`
- **Asset:** aUSDC (`0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c`)
- **Expected Balance:** 1233256387 (from contract)
- **Calculated Balance:** 1232132206 (too low by 1124181)

### Event Sequence (Simplified)

Each of the 16 cycles follows this pattern:
| logIndex | Event | User | Amount | Notes |
|----------|-------|------|--------|-------|
| 17 | SUPPLY | user | ~70,000 raw USDC | Deposit via router |
| 18 | Mint | user | ~7,800,000 value<br>~7,800,000 balanceIncrease<br>scaledAmount=0 | Interest accrual before transfer |
| 19 | Transfer | user → router | ~1,620,000,000 | Send to router |
| 20 | BalanceTransfer | user → router | ~1,620,000,000 | Record transfer |

The Mint at log 18 has `scaledAmount=0` in the event data because it's pure interest accrual before the transfer. The actual deposit amount comes from the SUPPLY event at log 17.

## Fix

### File: `src/degenbot/cli/aave.py`

**Location:** Lines 3066-3076 in `_process_collateral_mint_event()`

**Current Code:**
```python
# When value == balance_increase, validate that the calculated scaled amount
# equals the event value. If not, this SUPPLY event doesn't match this Mint
# event (e.g., the Mint is pure interest accrual before a transfer, not a deposit).
# This prevents incorrectly matching unrelated SUPPLY events.
# ref: Bug #0024
if event_amount == balance_increase and calculated_scaled_amount != event_amount:
    # This is not a matching SUPPLY event - it's pure interest accrual
    matched_pool_event = None
    scaled_amount = None
else:
    scaled_amount = calculated_scaled_amount
```

**Problem:** This check assumes that if `calculated_scaled_amount != event_amount`, the SUPPLY event must be for a different Mint. However, in ParaSwap transactions, the SUPPLY raw amount is intentionally different from the Mint event's value due to the multi-hop nature.

**Solution:** Instead of discarding the SUPPLY match when amounts don't align, use the calculated_scaled_amount from the SUPPLY event. The EventMatcher's consumption tracking already prevents double-counting by marking consumed SUPPLY events.

**Proposed Fix:**
```python
# When value == balance_increase and we have a SUPPLY match, use the
# calculated_scaled_amount even if it doesn't match event_amount exactly.
# The SUPPLY event's raw amount converted via rayDiv gives the correct
# scaled balance delta. EventMatcher's consumption tracking prevents
# double-counting already-consumed SUPPLY events.
# ref: Bug #0024, Bug #0026
scaled_amount = calculated_scaled_amount
```

**Alternative Solution (More Conservative):**

If the above causes issues with double-counting, a more conservative fix would be to only apply the Bug #0024 check when there's evidence the SUPPLY event is from a future Mint (e.g., SUPPLY.logIndex > Mint.logIndex):

```python
# When value == balance_increase, check if the SUPPLY event is from a
# future Mint. If so, discard it to avoid double-counting.
# ref: Bug #0024, Bug #0026
if (event_amount == balance_increase and 
    calculated_scaled_amount != event_amount and
    matched_pool_event["logIndex"] > event["logIndex"]):
    # This SUPPLY event is from a future Mint - discard to avoid double-counting
    matched_pool_event = None
    scaled_amount = None
else:
    scaled_amount = calculated_scaled_amount
```

## Key Insight

**Multi-hop transactions have different event patterns:** In ParaSwap and other aggregator transactions, the SUPPLY event amount may intentionally differ from the Mint event value due to:
1. Intermediate swaps (e.g., USDC → WETH)
2. Router fees
3. Slippage adjustments
4. Multiple hops within a single transaction

The Bug #0024 check was designed for simple SUPPLY → Mint sequences where amounts should match. It fails for complex multi-hop transactions where the amounts intentionally differ.

**The EventMatcher is the proper place to prevent double-counting:** The EventMatcher already tracks consumed SUPPLY events via `matched_pool_events`. If a SUPPLY event was already consumed by a previous Mint, it won't be available for matching. The Bug #0024 check is redundant and overly aggressive.

## Verification

To verify the fix:
1. Run `degenbot aave update --one-chunk --chunk 1` for block 18145516
2. Verify that user `0xAf8Eb92B802503A4737F6fBa38B9D734cb22A28b` balance matches contract
3. Expected: `position.balance == 1233256387`

## Refactoring

1. **Add transaction pattern detection:** Consider detecting ParaSwap and similar multi-hop transactions by checking for:
   - Multiple SUPPLY/BURN events in one transaction
   - Presence of router/adapter contracts
   - Complex event patterns (SUPPLY → Mint → Transfer → Burn → BalanceTransfer)

2. **Improve event matching:** The EventMatcher should use amount-based validation in addition to address matching. If amounts differ significantly (e.g., >10%), the match should be rejected.

3. **Document event patterns:** Create a reference document showing common event patterns for different transaction types:
   - Direct supply: SUPPLY → Mint
   - Router deposit: SUPPLY → Mint → Transfer
   - Multi-hop repay: Multiple (SUPPLY → Mint → Transfer → Burn) cycles
   - Pure interest: Mint (no SUPPLY)

4. **Add transaction-level validation:** After processing all events in a transaction, verify that the sum of balance changes across all users equals zero (conservation of aTokens).

## References

- Transaction: [0xc65753ab5751d591e08ac7a89910b129dbe3d09e9fcc94e32fc0a8d9a0da07a9](https://etherscan.io/tx/0xc65753ab5751d591e08ac7a89910b129dbe3d09e9fcc94e32fc0a8d9a0da07a9)
- Block: [18145516](https://etherscan.io/block/18145516)
- Related Issues: Bug #0021, Bug #0024, Bug #0019
- aToken Contract: [0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c](https://etherscan.io/token/0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c)
