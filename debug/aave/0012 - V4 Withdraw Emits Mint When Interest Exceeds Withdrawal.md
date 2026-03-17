# Issue: V4 Withdraw Emits Mint When Interest Exceeds Withdrawal

## Issue ID
**0012**

## Date
2026-03-15

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...). User AaveV3User(...) scaled balance (248704110774174174384791) does not match contract balance (248704110774174174384790) at block 23093217
```

**Balance Difference:** 1 wei (contract balance is 1 wei less than calculated balance)

**Log Output:**
```
WITHDRAW: Looking for collateral burn for user=0xB05aA33D347a3437E02B900Ba7a1981739A28E15, amount=10000000000000000000
WITHDRAW: Total scaled events: 2
WITHDRAW: Assigned indices: set()
WITHDRAW: Skipping logIndex=216 - type=ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER
WITHDRAW: Skipping logIndex=217 - type=ScaledTokenEventType.COLLATERAL_MINT
WITHDRAW: Found 0 collateral burns and 1 interest mints
```

## Root Cause
The Aave V4 aToken contract emits a **Mint event** instead of a Burn event when the interest accrued exceeds the withdrawal amount. The current code expects WITHDRAW operations to always have a COLLATERAL_BURN event, but this edge case results in a COLLATERAL_MINT event being emitted.

### Smart Contract Behavior (AToken rev_4.sol:2836-2843)

In the `_burnScaled` function:
```solidity
function _burnScaled(
  address user,
  address target,
  uint256 amountScaled,
  uint256 index,
  function(uint256, uint256) internal pure returns (uint256) getTokenBalance
) internal returns (bool) {
    uint256 scaledBalance = super.balanceOf(user);
    uint256 nextBalance = getTokenBalance(scaledBalance - amountScaled, index);
    uint256 previousBalance = getTokenBalance(scaledBalance, _userState[user].additionalData);
    uint256 balanceIncrease = getTokenBalance(scaledBalance, index) - previousBalance;

    _userState[user].additionalData = index.toUint128();

    _burn(user, amountScaled.toUint120());

    if (nextBalance > previousBalance) {
      // INTEREST EXCEEDS WITHDRAWAL - MINT instead of burn
      uint256 amountToMint = nextBalance - previousBalance;
      emit Transfer(address(0), user, amountToMint);
      emit Mint(user, user, amountToMint, balanceIncrease, index);
    } else {
      // NORMAL WITHDRAWAL - Burn
      uint256 amountToBurn = previousBalance - nextBalance;
      emit Transfer(user, address(0), amountToBurn);
      emit Burn(user, target, amountToBurn, balanceIncrease, index);
    }
    return scaledBalance - amountScaled == 0;
}
```

### Why This Happens

When a user withdraws collateral:
1. Interest accrues since their last interaction
2. The contract burns the scaled amount of aTokens
3. If `nextBalance > previousBalance`, the interest earned exceeds the withdrawal amount
4. Result: The user's balance still increases net-net, so a Mint event is emitted

**In this transaction:**
- Withdrawal amount: 10 USDS (10,000,000,000,000,000,000 wei)
- Interest accrued: ~0.725 aUSDS
- Net result: User's balance increases by ~0.165 aUSDS
- Contract emits: **Mint** event instead of Burn

## Transaction Details

| Field | Value |
|-------|-------|
| **Transaction Hash** | 0x50de2f56c6960b7f4a06b48d18fe5d7d8b1631bdde084f213a2555c934b44c18 |
| **Block Number** | 23093217 |
| **Chain** | Ethereum Mainnet |
| **User** | 0xB05aA33D347a3437E02B900Ba7a1981739A28E15 |
| **Asset** | USDS (0x4c9EDD5852cd905f086C759E8383e09bff1E68B3) |
| **aToken** | aUSDS (0x4F5923Fc5FD4a93352581b38B7cD26943012DECF) |
| **vToken** | 0x015396E1F286289aE23a762088E863b3ec465145 |
| **Pool** | 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 |
| **aToken Revision** | 4 |
| **Implementation** | 0xb76cf0f1d2e1a606c14044607c8c44878aae7186 |

### Events Emitted (in order):

1. **ReserveDataUpdated** (Pool, logIndex=213)
2. **Transfer** (USDS, logIndex=214) - 10 USDS from Pool to user
3. **Transfer** (aUSDS, logIndex=216) - ERC20 transfer (not used)
4. **Mint** (aUSDS, logIndex=217) - Interest accrual mint
   - `onBehalfOf`: 0xB05aA33D347a3437E02B900Ba7a1981739A28E15
   - `value`: 1606923735315744409 (~1.607 aUSDS)
   - `balanceIncrease`: 1606923735315744409
   - `index`: 1046959429379552160082961545
5. **Withdraw** (Pool, logIndex=219)

### Key Observation
The Mint event at logIndex 217 has:
- `value` (amount minted): 1606923735315744409
- `balanceIncrease` (accrued interest): 1606923735315744409
- These values are **equal**, which typically indicates pure interest accrual
- **However**, this Mint is part of a WITHDRAW operation, not standalone interest accrual

The scaled amount withdrawn was approximately 9547520495414 scaled units (10 USDS / index), but the interest accrued (~0.725 aUSDS worth) exceeded this, resulting in a net mint.

## Execution Path Analysis

### Current Code Flow (Failing)

1. **Operation Creation** (`aave_transaction_operations.py:1116-1177`)
   - Searches for `COLLATERAL_BURN` events matching the withdraw
   - Only looks for `ev.event_type == ScaledTokenEventType.COLLATERAL_BURN`
   - Skips `COLLATERAL_MINT` at logIndex=217
   - Finds 0 collateral burns
   - Falls back to creating operation with interest mint only

2. **Event Matching** (`aave_event_matching.py:143-151`)
   - Matches the Mint event to WITHDRAW operation
   - Returns `(pool_event, True)` - consumes the pool event

3. **Processing** (`aave.py:2526-2534`)
   - Routes to `_process_collateral_mint_with_match`
   - Calculates scaled_amount using standard TokenMath
   - **Problem:** The Mint event's amount represents the net mint, not a deposit

4. **Balance Verification** (`aave.py:1973`)
   - Compares calculated balance with on-chain balance
   - Fails: calculated balance is 1 wei higher than contract

### The 1 Wei Discrepancy

The contract balance is 248704110774174174384790, but calculated is 248704110774174174384791.

This occurs because:
- The Mint event's `value` field (1606923735315744409) is being added to the balance
- But this represents the net balance **increase**, not the withdrawal amount
- The underlying 10 USDS was still withdrawn from the contract
- The code doesn't properly account for this "withdrawal with net mint" scenario

## Fix

### Location
`src/degenbot/cli/aave_transaction_operations.py`

### Current Code (lines 1129-1147)
```python
if ev.event_type != ScaledTokenEventType.COLLATERAL_BURN:
    logger.debug(
        f"WITHDRAW: Skipping logIndex={ev.event['logIndex']} - type={ev.event_type}"
    )
    continue
```

### Fix Strategy

**Option A: Accept COLLATERAL_MINT when no burn found**

Modify the WITHDRAW operation creation to also look for COLLATERAL_MINT events when no COLLATERAL_BURN is found:

```python
# After searching for collateral burns, if none found, look for mints
if not collateral_burns:
    for ev in scaled_events:
        if ev.event["logIndex"] in assigned_indices:
            continue
        if ev.event_type != ScaledTokenEventType.COLLATERAL_MINT:
            continue
        if ev.user_address != user:
            continue
        if ev.index is None:
            continue
        # Check if this mint is part of the withdraw by verifying
        # the Transfer event to the user matches the withdraw amount
        # This requires additional logic to match ERC20 transfers
```

**Option B: Handle "interest exceeds withdrawal" case explicitly**

When a WITHDRAW operation is created with only a COLLATERAL_MINT (no burn):
1. Mark it as a special "WITHDRAW_WITH_NET_MINT" operation type
2. In enrichment, calculate the actual scaled withdrawal amount differently
3. The balance change should be: `-(withdrawAmount / index) + (mintAmount)`

**Option C: Pre-calculate expected balance change**

In the operation creation phase:
1. If no burn found but there's a mint for the same user/asset
2. Calculate if `mintAmount > (withdrawAmount / index)`
3. If so, this is the "interest exceeds withdrawal" case
4. Calculate the net balance change as: `mintAmount - (withdrawAmount / index)`

### Recommended Fix: Option B with Operation Type Extension

1. **Modify `OperationType` enum** to include `WITHDRAW_WITH_NET_MINT`
2. **Update `_create_withdraw_operation`** to detect this case and set the appropriate operation type
3. **Update enrichment** to handle the balance calculation for this operation type
4. **Update processing** in `aave.py` to route to appropriate handler

## Key Insight

**The Mint Event is Not Interest Accrual - It's a Withdrawal Result**

Unlike INTEREST_ACCRUAL operations where Mint events are tracking-only (see Issue #0004), this Mint event **does** represent an actual balance change. The user's scaled balance increases because:

1. Interest accrues (increasing the balance)
2. Withdrawal executes (decreasing the balance)
3. Net result: Balance still increases → Mint emitted

**The key difference:**
- **INTEREST_ACCRUAL mints**: `scaled_amount = 0` (tracking only, no balance change)
- **WITHDRAW with net mint**: `scaled_amount > 0` (actual balance increase from interest exceeding withdrawal)

## Refactoring

1. **Operation Classification**: Add handling for withdrawals that result in net mints when interest exceeds withdrawal amount. This is a distinct case from both standard withdrawals (burn) and pure interest accrual.

2. **Event Matching Logic**: Update `_create_withdraw_operation` to look for COLLATERAL_MINT events when no COLLATERAL_BURN is found, and validate the mint belongs to the withdraw by checking associated ERC20 transfers.

3. **Balance Calculation**: For "withdraw with net mint" operations:
   - Calculate scaled withdrawal amount: `withdrawAmount / index`
   - Subtract from balance
   - Add mint amount to balance
   - Net change: `mintAmount - (withdrawAmount / index)`

4. **Validation**: Add a check to ensure the Mint event's `value` equals `balanceIncrease` (confirms this is interest-related, not a deposit).

5. **Documentation**: Add comments referencing the Aave V4 aToken contract behavior where `_burnScaled` can emit Mint instead of Burn when `nextBalance > previousBalance`.

## Related Issues

- Issue #0004: Interest Accrual Scaling Error in Enrichment - Similar Mint event handling, but for pure interest accrual
- Issue #0008: INTEREST_ACCRUAL Debt Burn Missing Pool Event Reference - Related to interest accrual edge cases
- IAToken.sol:75 - "In some instances, a mint event may be emitted from a burn transaction if the amount to burn is less than the interest that the user accrued"

## Files Involved

- `src/degenbot/cli/aave_transaction_operations.py` - Lines 1116-1236 (WITHDRAW operation creation)
- `src/degenbot/cli/aave_event_matching.py` - Lines 143-151 (_match_withdraw)
- `src/degenbot/cli/aave.py` - Lines 2526-2534 (event routing)
- `contract_reference/aave/AToken/rev_4.sol` - Lines 2818-2847 (_burnScaled function)

## Verification Steps

After implementing fix:
```bash
# Run specific block
uv run degenbot aave update --chunk 1

# Verify balance
uv run python3 -c "
from web3 import Web3
w3 = Web3(Web3.HTTPProvider('http://node:8545'))
# Check scaled balance at block 23093217
result = w3.eth.call({
    'to': '0x4F5923Fc5FD4a93352581b38B7cD26943012DECF',
    'data': '0x1da8d368000000000000000000000000B05aA33D347a3437E02B900Ba7a1981739A28E15',
}, block_identifier=23093217)
print('Scaled balance:', int(result.hex(), 16))
"
```

## Notes

- This behavior is specific to Aave V4 aTokens (and likely V5+ which uses the same TokenMath)
- The "mint on withdrawal" case is rare but legitimate - it happens when a user has significant accrued interest relative to their withdrawal amount
- The IAToken interface explicitly documents this: "In some instances, a mint event may be emitted from a burn transaction if the amount to burn is less than the interest that the user accrued."
