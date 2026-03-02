# Issue 0016: Collateral Index Mismatch in Liquidation with Multiple Operations

**Date:** 2026-03-02

## Symptom
```
AssertionError: User 0xcC717037652940F319272B0bF57591e41d157F95: collateral last_index (1001331979729743704723436158) does not match contract (1001331991252538377398964857) @ 0xCc9EE9483f662091a1de4795249E24aC0aC2630f at block 19695735
```

## Root Cause

This is a complex liquidation transaction where the user has multiple scaled token operations:

1. **Mint Event** (log index 0x2d/45): User receives aTokens via mint with index = 1001331979729743704723436158
2. **Burn Event** (log index 0x40/64): User burns aTokens with index = 1001331991252538377398964857

The burn event has a **higher index** than the mint event and occurs later in the transaction. According to the AToken contract, every mint/burn/transfer operation updates the user's `additionalData` (which stores the last index). Therefore, the user's `last_index` should be updated to the burn event's index.

The database has the mint's index, but the contract has the burn's index, indicating the burn event is not being processed correctly for this user.

### Why This Happens

The transaction contains a LIQUIDATION_CALL event (log index 0x2a/42) where:
- User being liquidated: 0xcC717037652940F319272B0bF57591e41d157F95
- Collateral: WETH
- Debt: Variable debt for the user

The burn event at log index 0x40 is the collateral burn for this liquidation. The burn event has:
- `from`: 0xcC717037652940F319272B0bF57591e41d157F95 (the user)
- `target`: 0xcC717037652940F319272B0bF57591e41d157F95 (the user)
- Index: 1001331991252538377398964857

## Transaction Details

- **Transaction Hash:** 0xee298f9bffbad5ea30fbae09492ef66de30af76d29e3e19f5b076e3a8944901e
- **Block:** 19695735
- **User:** 0xcC717037652940F319272B0bF57591e41d157F95
- **AToken:** 0xCc9EE9483f662091a1de4795249E24aC0aC2630f (aRETH)
- **Type:** Liquidation with multiple scaled token operations

## Events Sequence

1. Log 0x2d (45): Mint(aRETH, user=0xcC717..., index=1001331979729743704723436158)
2. Log 0x2a (42): LiquidationCall(collateral=WETH, debt=variable, user=0xcC717...)
3. Log 0x40 (64): Burn(aRETH, from=user, target=user, index=1001331991252538377398964857)

## Expected Behavior

The user's `last_index` should be updated to the burn event's index (1001331991252538377398964857) because:
1. The burn event occurs after the mint event
2. The burn event has a higher index
3. The AToken contract updates `_userState[user].additionalData` to the current index on every burn operation

## Actual Behavior

The database stores the mint event's index (1001331979729743704723436158), causing a mismatch with the contract.

## Key Insight

Liquidation transactions can have multiple scaled token events (mints, burns, transfers) affecting the same user. Each of these events can have a different index value, and the last one processed should determine the final `last_index` value stored in the database.

The issue is likely in how the liquidation operation matches and processes the collateral burn event for the liquidated user.

## Refactoring Recommendations

1. **Verify event ordering:** Ensure scaled token events within a liquidation operation are processed in chronological order (by log index).

2. **Audit operation matching:** Review `_create_liquidation_operation` to verify that all collateral burns for the liquidated user are correctly matched and included in the operation's `scaled_token_events`.

3. **Add comprehensive logging:** Include log index information when processing scaled token events to help diagnose ordering issues.

4. **Consider transaction-level index tracking:** Instead of relying on individual operation processing, track the highest index seen for each user within a transaction and apply it at the end.

## Investigation Commands

```bash
# Check contract index at block
cast call 0xCc9EE9483f662091a1de4795249E24aC0aC2630f \
  "getPreviousIndex(address)(uint256)" \
  0xcC717037652940F319272B0bF57591e41d157F95 \
  --block 19695735

# View transaction trace
cast run 0xee298f9bffbad5ea30fbae09492ef66de30af76d29e3e19f5b076e3a8944901e
```

## Related Events

- Mint Event (log 0x2d): `0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196`
- Burn Event (log 0x40): `0x4cf25bc1d991c17529c25213d3cc0cda295eeaad5f13f361969b12ea48015f90`
- LiquidationCall (log 0x2a): `0xa534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051`
