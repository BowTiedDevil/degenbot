# Issue 0024: Umbrella Staking Collateral Burn Without Pool Event

**Date:** 2026-03-04

## Symptom

```
AssertionError: User 0xD400fc38ED4732893174325693a63C30ee3881a8: collateral balance (148831959) does not match scaled token contract (0) @ 0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c at block 22638170
```

## Root Cause

Unassigned `COLLATERAL_BURN` events were not being processed. When aTokens are burned without a corresponding `WITHDRAW`, `REPAY`, or `LIQUIDATION_CALL` pool event (such as during Aave Umbrella staking contract creation), the burn event was not matched to any operation and was silently skipped.

The transaction at block 22638170 involved Aave Umbrella staking contract deployment (`stkwaEthUSDC`), which performed:
1. Transfer IN from Pool to new contract (log 19)
2. BalanceTransfer to track the scaled balance (log 21) 
3. Transfer OUT from contract to zero address (log 23)
4. Burn event to burn the aTokens (log 24)

The Transfer IN was processed using the BalanceTransfer amount (148831959), but the Burn event (log 24) was never processed because it had no matching pool event, leaving the database balance at 148831959 instead of 0.

## Transaction Details

- **Hash:** `0xaa900e1ac9ece8a1a0db38c111ccfe5b5fb735a838278995a7e6534a8fc32a63`
- **Block:** 22638170
- **Type:** Aave Governance Payload Execution (Umbrella staking contract deployment)
- **User:** `0xD400fc38ED4732893174325693a63C30ee3881a8` (Aave Umbrella: stkwaEthUSDC.v1 Token contract)
- **Asset:** USDC (aEthUSDC at `0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c`)

**Event Sequence:**
- Log 19: Transfer (Pool → User, amount 168401963)
- Log 21: BalanceTransfer (Pool → User, scaled amount 148831959)
- Log 23: Transfer (User → 0x0, amount 168401963) 
- Log 24: Burn (User, amount 168401963)

## Fix

**File:** `src/degenbot/cli/aave_transaction_operations.py`

**Location:** `_create_interest_accrual_operations` method (lines 1301-1316)

**Change:** Added handling for unassigned `COLLATERAL_BURN` events, similar to how unassigned `DEBT_BURN` events are handled:

```python
# Handle unassigned collateral burn events
# These can occur in umbrella/staking operations where aTokens are
# burned without a corresponding WITHDRAW pool event (e.g., stkwaEthUSDC creation)
if ev.event_type == "COLLATERAL_BURN":
    operations.append(
        Operation(
            operation_id=operation_id,
            operation_type=OperationType.INTEREST_ACCRUAL,
            pool_event=None,
            scaled_token_events=[ev],
            transfer_events=[],
            balance_transfer_events=[],
        )
    )
    operation_id += 1
    continue
```

The fix creates an `INTEREST_ACCRUAL` operation for standalone collateral burn events, allowing them to be processed and reducing the user's collateral balance appropriately.

## Key Insight

When processing Aave V3 events, not all scaled token burns correspond to pool operations. Some burns (like those during umbrella/staking contract creation) are direct burns of aTokens without a `WITHDRAW`, `REPAY`, or `LIQUIDATION_CALL` event. These must still be processed to maintain accurate collateral balances.

The existing code already handled this case for `DEBT_BURN` events (flash loans), but `COLLATERAL_BURN` events were overlooked.

## Refactoring

The `_create_interest_accrual_operations` method should potentially be renamed to `_create_unmatched_scaled_token_operations` or similar, as it now handles:
1. Interest accrual mints
2. Unassigned debt burns (flash loans)
3. Unassigned collateral burns (umbrella/staking)

The `OperationType.INTEREST_ACCRUAL` name is also misleading since it now handles burn events as well. Consider creating a more generic operation type like `STANDALONE_SCALED_TOKEN_OPERATION`.

## References

- Transaction: https://etherscan.io/tx/0xaa900e1ac9ece8a1a0db38c111ccfe5b5fb735a838278995a7e6534a8fc32a63
- Block: 22638170
- Token: aEthUSDC (0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c)
- User: stkwaEthUSDC vault (0xD400fc38ED4732893174325693a63C30ee3881a8)
