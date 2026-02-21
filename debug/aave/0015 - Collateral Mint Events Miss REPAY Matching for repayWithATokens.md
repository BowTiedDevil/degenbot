---
title: Collateral Mint Events Miss REPAY Matching for repayWithATokens
category: aave
tags:
  - aave
  - event-matching
  - repay-with-atokens
  - bug
complexity: standard
---

# Issue: Collateral Mint Events Miss REPAY Matching for repayWithATokens

## Date
2025-02-20

## Symptom
```
No matching event found for COLLATERAL_MINT. Tried users: ['0x8899fAEd2e1b0e9b7F41E08b79bE71eC3d1f9EC1', '0x8899fAEd2e1b0e9b7F41E08b79bE71eC3d1f9EC1'], reserve: 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2. Available pool events: 1

ValueError: No matching Pool event for collateral mint in tx 31dff4015dffaa1511977e04ed37150a05c9894098ecef1fe6f9870a5dbdc442. User: 0x8899fAEd2e1b0e9b7F41E08b79bE71eC3d1f9EC1, Reserve: 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2. Available: ['a534c8dbe7']
```

## Root Cause

The `repayWithATokens()` function in Aave V3 allows users to repay their debt using aTokens instead of underlying tokens. When the amount of aTokens used for repayment exceeds the actual debt (due to interest accrual or over-repayment), the excess aTokens are minted back to the user as a collateral mint.

In this scenario:
1. **LogIndex 51**: VariableDebtToken Burn event (debt repayment)
2. **LogIndex 54**: AToken Mint event (excess aTokens returned to user) - **COLLATERAL_MINT**
3. **LogIndex 55**: Pool Repay event

The `COLLATERAL_MINT` MatchConfig only looked for:
- `SUPPLY` - deposit events
- `WITHDRAW` - withdrawal events
- `LIQUIDATION_CALL` - liquidation events

It did **not** include `REPAY` events, causing the collateral mint from `repayWithATokens()` to fail to find a matching Pool event.

## Transaction Details

- **Hash**: `0x31dff4015dffaa1511977e04ed37150a05c9894098ecef1fe6f9870a5dbdc442`
- **Block**: 22073161
- **Chain**: Ethereum mainnet (chain_id: 1)
- **Function**: `repayWithATokens(address,uint256,uint256)`
- **User**: `0x8899fAEd2e1b0e9b7F41E08b79bE71eC3d1f9EC1`
- **Asset (Reserve)**: WETH (`0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2`)
- **Event signature 'a534c8dbe7'**: `Repay(address indexed reserve, address indexed user, address indexed repayer, uint256 amount, bool useATokens)`

### Event Flow

| LogIndex | Event | Contract | Description |
|----------|-------|----------|-------------|
| 51 | Burn | VariableDebtToken | Burns 7,362,288,326,168 scaled debt tokens (repayment) |
| 54 | Mint | A_WETH Token | **COLLATERAL_MINT** - Mints excess aTokens back to user |
| 55 | Repay | Aave Pool | `Repay(reserve=WETH, user=..., amount=..., useATokens=true)` |

## Fix

**File**: `src/degenbot/cli/aave_event_matching.py`

### Change 1: Add REPAY to COLLATERAL_MINT pool_event_types

```python
ScaledTokenEventType.COLLATERAL_MINT: MatchConfig(
    target_event=ScaledTokenEventType.COLLATERAL_MINT,
    pool_event_types=[
        AaveV3Event.SUPPLY,
        AaveV3Event.WITHDRAW,
        AaveV3Event.REPAY,  # <-- ADDED
        AaveV3Event.LIQUIDATION_CALL,
    ],
    consumption_policy=EventConsumptionPolicy.CONDITIONAL,
    consumption_condition=lambda e: _should_consume_collateral_mint_pool_event(e),
),
```

### Change 2: Update consumption logic to not consume REPAY events

```python
def _should_consume_collateral_mint_pool_event(pool_event: LogReceipt) -> bool:
    event_topic = pool_event["topics"][0]

    # LIQUIDATION_CALL and REPAY are never consumed because they must be available
    # to match multiple operations (liquidations or repay-with-aTokens transactions)
    if event_topic in {
        AaveV3Event.LIQUIDATION_CALL.value,
        AaveV3Event.REPAY.value,  # <-- ADDED
    }:
        return False

    # SUPPLY and WITHDRAW are consumable
    return True
```

**Rationale**: The REPAY event should not be consumed by the collateral mint because it also needs to match the debt burn event in the same transaction. This is similar to how LIQUIDATION_CALL is handled - it's a shared event across multiple scaled token operations.

## Key Insight

`repayWithATokens()` is a unique Aave operation that:
1. Burns variable debt tokens (debt reduction)
2. May mint collateral tokens (if over-repayment or interest-related excess)
3. Emits a single REPAY event covering both operations

This is distinct from regular repayments where:
- If `useATokens=false`: User transfers underlying tokens, debt tokens are burned, no collateral mint
- If `useATokens=true` with exact amount: aTokens are transferred, debt tokens burned, no collateral mint
- If `useATokens=true` with excess: aTokens transferred, debt tokens burned, **excess aTokens minted back as collateral**

The event matching framework must account for this third case where REPAY events can be associated with both debt burns AND collateral mints in the same transaction.

## Refactoring

1. **Add explicit test cases** for `repayWithATokens()` scenarios including:
   - Exact repayment (no excess)
   - Over-repayment with excess aTokens returned
   - Multiple asset repayments

2. **Improve error messages** to suggest potential missing match configurations when events can't be matched

3. **Document the event flow** for `repayWithATokens()` in the module docstring to help future developers understand the relationship between:
   - Debt burns (VariableDebtToken Burn)
   - Collateral mints (AToken Mint for excess)
   - Single REPAY event covering both

4. **Consider adding transaction-level context** to the event matcher to track when a REPAY event has been "partially consumed" by a debt burn, which would indicate it should also match a collateral mint.

## References

- Transaction: [0x31dff4015dffaa1511977e04ed37150a05c9894098ecef1fe6f9870a5dbdc442](https://etherscan.io/tx/0x31dff4015dffaa1511977e04ed37150a05c9894098ecef1fe6f9870a5dbdc442)
- Block: [22073161](https://etherscan.io/block/22073161)
- Aave V3 Pool Contract: `0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2`
- Related Issues: #0008, #0010, #0012
