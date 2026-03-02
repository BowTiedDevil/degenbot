# Issue 0017: GHO Discount Calculation with Stale stkAAVE Balance

**Date:** 2026-03-02

## Symptom

```
AssertionError: User 0x329c54289Ff5D6B7b7daE13592C6B1EDA1543eD4: GHO discount 1273 does not match GHO vDebtToken contract (1276) @ 0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B at block 19325561
```

The tracked GHO discount was 3 basis points (0.03%) lower than the actual contract value.

## Root Cause

When processing a transaction, operations (like GHO debt mints) were processed BEFORE non-operation events (like stkAAVE transfers). This caused GHO discount calculations to use stale stkAAVE balances.

### Event Order Problem

In the failing transaction at block 19325561 (tx: 0xaef7ff6293c8906bf0736fe5bb997445c6cd49c54cb1d082922f7c3b26515470), events occurred in this order:

1. **logIndex 179:** stkAAVE Transfer (reward claim - receiving stkAAVE)
2. **logIndex 186:** stkAAVE Transfer (staking AAVE - receiving more stkAAVE)
3. **logIndex 188:** GHO VariableDebtToken Mint (discount update triggered)

The GHO mint at logIndex 188 calculates a new discount rate based on the user's stkAAVE balance and debt balance. However, the code was processing operations first, then non-operation events, so:

1. GHO mint was processed with OLD stkAAVE balance
2. stkAAVE transfers were processed afterwards (too late)

### Technical Details

The user's stkAAVE balance changed from pre-transaction to post-transaction:
- Pre-transaction balance: ~2.596e21
- Post-transaction balance: ~2.602e21 (after receiving rewards and staking)

The 3 bps discount difference (1273 vs 1276) was caused by using the pre-transaction stkAAVE balance in the discount calculation formula:

```python
discount = (stkAAVE_balance * 100 * 3000) // debt_balance
```

With the stale balance: 1273 bps  
With the correct balance: 1276 bps

## Transaction Details

**Failing Block:** 19325561  
**Transaction Hash:** 0xaef7ff6293c8906bf0736fe5bb997445c6cd49c54cb1d082922f7c3b26515470  
**Failing User:** 0x329c54289Ff5D6B7b7daE13592C6B1EDA1543eD4  
**Transaction Type:** Rewards claim + stake on stkAAVE contract  
**GHO vDebtToken:** 0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B

**Event Sequence:**
1. User claims stkAAVE rewards (Transfer from 0x0 to user)
2. User stakes AAVE tokens (receives stkAAVE)
3. GHO contract mints dust debt (triggered by discount rate recalculation)

## Fix

**File:** `src/degenbot/cli/aave.py`

**Change:** Process stkAAVE transfers BEFORE operations in `_process_transaction()`.

The fix adds a pre-processing loop for stkAAVE transfers before the operations loop:

```python
# Process stkAAVE transfers BEFORE operations to ensure stkAAVE balances
# are up-to-date when GHO debt operations calculate discount rates.
# This handles cases where stkAAVE transfers (e.g., rewards claims) occur
# before GHO mint/burn events in the same transaction.
if gho_asset and gho_asset.v_gho_discount_token:
    discount_token = gho_asset.v_gho_discount_token
    for event in tx_context.events:
        topic = event["topics"][0]
        event_address = get_checksum_address(event["address"])
        if (
            topic == ERC20Event.TRANSFER.value
            and event_address == discount_token
        ):
            _process_stk_aave_transfer_event(...)

# Then process operations (with updated stkAAVE balances)
for operation in sorted_operations:
    _process_operation(...)

# Finally process remaining non-operation events (excluding stkAAVE transfers)
```

The stkAAVE transfer processing was also removed from the non-operation events loop to prevent double-counting.

## Key Insight

**Event processing order matters for inter-dependent state.** When multiple contracts interact in a single transaction (like stkAAVE transfers affecting GHO discount calculations), the processing order must respect the actual event order in the transaction.

**The Aave V3 GHO discount mechanism recalculates on every state change.** The GHO VariableDebtToken contract recalculates the discount rate whenever the user's stkAAVE balance or debt balance changes. This means:
- stkAAVE transfers trigger discount recalculation
- Debt mints/burns trigger discount recalculation  
- The discount must be calculated using the CURRENT balances, not stale ones

## Refactoring Recommendations

1. **Event Dependency Graph:** Consider building a dependency graph for events to ensure they're processed in the correct order based on their interdependencies (e.g., stkAAVE transfers → GHO operations).

2. **Balance Snapshotting:** When processing operations that depend on balances (like GHO discount calculations), consider fetching fresh balance values from the contract at the specific block to ensure accuracy.

3. **Transaction State Machine:** Track the evolving transaction state (balances, indices, discounts) as events are processed, ensuring each operation sees the correct cumulative state.

4. **Test Coverage:** Add tests for transactions with multiple contract interactions (e.g., stkAAVE transfers + GHO operations in the same transaction).

## References

- Aave V3 GhoDiscountRateStrategy contract: `0x4C38Ec4D1D2068540DfC11DFa4de41F733DDF812`
- GHO VariableDebtToken contract: `0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B`
- stkAAVE contract: `0x4da27a545c0c5B758a6BA100e3a049001de870f5`
- Related issues: Issue 0008 (Stale Debt Index) - similar root cause of stale state
