# Issue: WITHDRAW Interest Accrual Mint Matching with Rounding Tolerance

## Issue ID
**0013**

## Date
2026-03-15

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), underlying_token=Erc20TokenTable(chain=1, address='0x4c9EDD5852cd905f086C759E8383e09bff1E68B3', symbol=None), a_token=Erc20TokenTable(chain=1, address='0x4F5923Fc5FD4a93352581b38B7cD26943012DECF', symbol=None), v_token=Erc20TokenTable(chain=1, address='0x015396E1F286289aE23a762088E863b3ec465145', symbol=None)). User AaveV3User(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), address='0xef9bb2F993631D4a8E36548Ab722B081087176b8', e_mode=2) scaled balance (954103406497862961842488) does not match contract balance (954103406497862961842489) at block 23094514
```

**Balance Difference:** 1 wei (contract balance is 1 wei more than calculated balance)

## Root Cause
When matching interest accrual mint events to WITHDRAW operations, the code at line 1110 in `aave_transaction_operations.py` requires an **exact match** between the withdraw amount and `(balance_increase - mint_amount)`. However, due to **ray math rounding** in the Aave V3 contracts, these values can differ by ±1 wei.

### The Rounding Problem

In this transaction:
- **Withdraw amount:** 1,000,000,000,000,000,000 (1 USDe)
- **Mint event amount:** 23,398,321,819,863,665,647
- **Mint event balance_increase:** 24,398,321,819,863,665,648
- **Expected difference:** 24,398,321,819,863,665,648 - 23,398,321,819,863,665,647 = **1,000,000,000,000,000,001**

The calculated difference (1,000,000,000,000,000,001) differs from the actual withdraw amount (1,000,000,000,000,000,000) by **1 wei**.

### Why This Happens

The Aave V3 aToken contract uses **ray math** (27 decimal precision) with **round-half-up** for all calculations:

```solidity
// From AToken contract _burnScaled function
uint256 amountToBurn = amount + balanceIncrease;  // Underlying units
uint256 amountScaled = amountToBurn.rayDiv(index);  // Scaled units with rounding
```

When calculating the mint amount for the "interest exceeds withdrawal" case:
1. Interest accrues in underlying units: `balanceIncrease`
2. Withdrawal amount in underlying units: `amount`
3. Net mint amount: `balanceIncrease - amount`
4. But this is calculated from scaled balances, then converted back to underlying

The conversion introduces rounding: `underlying = scaled * index / RAY`

Since the Mint event's `amount` field is computed as `balanceIncrease - amount` in the contract, and both `balanceIncrease` and the internal withdrawal calculation involve rounding, the final `amount` can be off by ±1 wei from the expected value.

### Execution Flow (Failing)

1. **WITHDRAW Operation Creation** (`aave_transaction_operations.py:1097-1114`)
   - Searches for interest mints where `withdraw_amount == balance_increase - mint_amount`
   - Check fails due to 1 wei difference (1000000000000000000 != 1000000000000000001)
   - Mint event at logIndex 341 is NOT assigned to WITHDRAW operation

2. **Interest Accrual Operation Creation** (`aave_transaction_operations.py:625-642`)
   - Mint event at logIndex 341 is unassigned after WITHDRAW creation
   - Falls through to `_create_interest_accrual_operations`
   - Creates INTEREST_ACCRUAL operation with the mint event

3. **Event Enrichment** (`aave/enrichment.py:86-94`)
   - INTEREST_ACCRUAL operations set `scaled_amount = 0`
   - This is correct for pure interest accrual, but WRONG for this case

4. **Processing** (`aave.py:2619-2690`)
   - `_process_collateral_mint_with_match` processes the mint
   - But `scaled_amount` is 0, so no balance change
   - The mint event represents an actual balance increase that is not applied

5. **Balance Verification** (`aave.py:1973`)
   - Calculated balance is 1 wei less than contract balance
   - Fails assertion

## Transaction Details

| Field | Value |
|-------|-------|
| **Transaction Hash** | 0xc008cf173b19c4f6c1d02832daa7b7fb66de5c92e23d73ab7046f51c6f142556 |
| **Block Number** | 23094514 |
| **Chain** | Ethereum Mainnet |
| **User** | 0xef9bb2F993631D4a8E36548Ab722B081087176b8 |
| **Asset** | USDe (0x4c9EDD5852cd905f086C759E8383e09bff1E68B3) |
| **aToken** | aEthUSDe (0x4F5923Fc5FD4a93352581b38B7cD26943012DECF) |
| **vToken** | 0x015396E1F286289aE23a762088E863b3ec465145 |
| **Pool** | 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 |
| **aToken Revision** | 4 |
| **Pool Revision** | 9 |
| **Implementation** | 0xb76cf0f1d2e1a606c14044607c8c44878aae7186 |

### Events Emitted (in order):

1. **ReserveDataUpdated** (Pool, logIndex=339)
2. **Transfer** (aUSDe, logIndex=340) - Mint of 23,398,321,819,863,665,647 aUSDe
3. **Mint** (aUSDe, logIndex=341) - Interest accrual with withdrawal
   - `onBehalfOf`: 0xef9bb2F993631D4a8E36548Ab722B081087176b8
   - `value`: 23,398,321,819,863,665,647 (~23.398 aUSDe)
   - `balanceIncrease`: 24,398,321,819,863,665,648 (~24.398 aUSDe)
   - `index`: 1046979469202911397379125738
4. **Transfer** (USDe, logIndex=342) - 1 USDe from Pool to user
5. **Withdraw** (Pool, logIndex=343)

### Key Observation

The Mint event at logIndex 341 shows:
- `value` (net mint amount): 23,398,321,819,863,665,647
- `balanceIncrease` (accrued interest): 24,398,321,819,863,665,648
- Difference: 1,000,000,000,000,000,001 (should be exactly 1,000,000,000,000,000,000)

This 1 wei difference is the rounding error from ray math operations in the contract.

## Smart Contract Analysis

### AToken Revision 4 _burnScaled Function

```solidity
function _burnScaled(
  address user,
  address target,
  uint256 amount,
  uint256 index,
  function(uint256, uint256) internal pure returns (uint256) getTokenBalance
) internal returns (bool) {
    uint256 scaledBalance = super.balanceOf(user);
    uint256 nextBalance = getTokenBalance(scaledBalance - amount.rayDiv(index), index);
    uint256 previousBalance = getTokenBalance(scaledBalance, _userState[user].additionalData);
    uint256 balanceIncrease = getTokenBalance(scaledBalance, index) - previousBalance;

    _userState[user].additionalData = index.toUint128();

    _burn(user, amount.rayDiv(index).toUint120());

    if (nextBalance > previousBalance) {
      // Interest exceeds withdrawal
      uint256 amountToMint = nextBalance - previousBalance;  // This calculation introduces rounding
      emit Transfer(address(0), user, amountToMint);
      emit Mint(user, user, amountToMint, balanceIncrease, index);
    }
    // ...
}
```

The `amountToMint` calculation involves ray multiplication which rounds to the nearest integer, introducing ±1 wei variance.

## Fix

### Status
**IMPLEMENTED** - 2026-03-15

### Location
`src/degenbot/cli/aave_transaction_operations.py`

### Current Code (lines 1108-1112)
```python
if (
    ev.balance_increase is not None
    and withdraw_amount != ev.balance_increase - ev.amount
):
    continue
```

### Fix Applied (lines 1108-1115)
```python
if ev.balance_increase is not None:
    calculated_withdraw = ev.balance_increase - ev.amount
    # Pool revision 9+ uses ray math with rounding, allow ±2 wei tolerance
    if pool_revision >= 9:  # noqa:PLR2004
        if abs(calculated_withdraw - withdraw_amount) > 2:  # noqa:PLR2004
            continue
    elif calculated_withdraw != withdraw_amount:
        continue
```

### Why This Fix Works

The fix follows the established pattern used elsewhere in the codebase for Pool revision 9+ tolerance:
- **SUPPLY** (line 1021): Allows `(supply_amount - 2, supply_amount - 1, supply_amount)`
- **WITHDRAW burn** (line 1158): Allows `(withdraw_amount, withdraw_amount + 1)`  
- **REPAY** (line 1639): Allows `±2` wei deviation

Pool revision 9 introduced pre-scaling with ray division, which can introduce ±2 wei rounding variance due to:
1. Ray multiplication rounding to nearest: `(a * b + RAY/2) / RAY`
2. Multiple chained operations compounding rounding errors
3. Floor/ceiling division in TokenMath

## Verification

### Test Results
Running `uv run degenbot aave update --chunk 1` at block 23094514:

**Before Fix:**
```
WITHDRAW: Found 0 collateral burns and 0 interest mints
WITHDRAW at logIndex=343 has no matching burn/mint event, creating minimal operation
...
AssertionError: Balance verification failure... scaled balance (954103406497862961842488) 
does not match contract balance (954103406497862961842489)
```

**After Fix:**
```
WITHDRAW: Found 0 collateral burns and 1 interest mints
ENRICHMENT: Interest exceeds withdrawal - using withdraw amount 1000000000000000000 for burn calculation
ENRICHMENT: Interest exceeds withdrawal - using COLLATERAL_BURN calculation (ceil rounding)
_process_scaled_token_operation burn: delta=-955128566906208863, new_balance=954103406497862961842489
...
AaveV3Market successfully updated to block 23,094,514
```

✅ **Balance verification now passes** - calculated balance (954103406497862961842489) matches contract balance exactly.

### Additional Fix: WITHDRAW Burn Matching Tolerance

**Issue Found:** When testing with block 23108271, another 2 wei discrepancy was discovered:
- Withdraw amount: 2634819272587
- Burn event amount + balance_increase: 2634819272589
- Difference: 2 wei

**Fix Applied (line 1152-1167):**
```python
# Calculate expected burn amount(s)
expected_burn_amounts = (
    # Pool revision 9 began pre-scaling the amount with flooring ray division.
    # Calculating it exactly requires injecting extra details about the position,
    # so this check will allow up to a ±2 wei deviation on pool revisions 9+
    (
        withdraw_amount - 2,
        withdraw_amount - 1,
        withdraw_amount,
        withdraw_amount + 1,
        withdraw_amount + 2,
    )
    if pool_revision >= 9  # noqa:PLR2004
    else {withdraw_amount}
)
```

**Why ±2 is needed:**
The original tolerance was `(withdraw_amount, withdraw_amount + 1)`, allowing only 0 or +1 deviation. However, due to ray math asymmetry between `rayMulFloor` and `rayDivCeil`, the actual Burn event can differ by up to ±2 wei from the Pool's withdraw amount.

### Additional Fix: Cross-Token INTEREST_ACCRUAL Burn Handling

**Issue Found:** The `last_withdraw_amount` was being used for ALL INTEREST_ACCRUAL burns regardless of token/user, causing incorrect balance calculations when a transaction had multiple withdrawals with different tokens.

**Fix Applied in `aave.py`:**

**1. Extended TransactionContext (`aave_types.py:88-90`):**
```python
last_withdraw_amount: int = 0
last_withdraw_token_address: ChecksumAddress | None = None
last_withdraw_user_address: ChecksumAddress | None = None
```

**2. Store Token/User Context (`aave.py:2351-2365`):**
```python
# Store the token and user addresses for matching with INTEREST_ACCRUAL burns
if operation.scaled_token_events:
    first_event = operation.scaled_token_events[0]
    tx_context.last_withdraw_token_address = get_checksum_address(
        first_event.event["address"]
    )
    tx_context.last_withdraw_user_address = first_event.user_address
```

**3. Match Before Using (`aave.py:2812-2822`):**
```python
# Only apply if the burn is for the same user AND token as the last WITHDRAW
if (
    operation.operation_type == OperationType.INTEREST_ACCRUAL
    and tx_context.last_withdraw_amount > 0
    and token_address == tx_context.last_withdraw_token_address
    and scaled_event.user_address == tx_context.last_withdraw_user_address
):
    raw_amount = tx_context.last_withdraw_amount
    logger.debug(
        f"Using stored WITHDRAW amount for INTEREST_ACCRUAL: {raw_amount} "
        f"(user={scaled_event.user_address}, token={token_address})"
    )
```

**Example:** Transaction 0xad32fae812e9f78570d003a11cf0ae4f1ab9e54e3f3bf6b38069aac1021650bd
- WITHDRAW 2634819272587 USDT (aEthUSDT)
- INTEREST_ACCRUAL burn 2634819272589 USDT (same token) ✓ matches
- WITHDRAW 4999999999999999999999 WETH (aEthWETH)
- INTEREST_ACCRUAL burn for USDT (different token) ✗ should NOT use WETH amount

Before the fix, the USDT INTEREST_ACCRUAL burn was incorrectly using the WETH withdraw amount (4999999999999999999999) instead of its own amount (2634819272589).

## Key Insight

**Ray Math Rounding is Non-Deterministic Across Operations**

The Aave V3 protocol uses ray math (27 decimal precision) with round-half-up for all interest calculations. When multiple ray operations are chained:

1. `scaled * index / RAY` → rounds to nearest
2. Multiple such operations compound rounding errors
3. Final results can differ by ±1-2 wei from "true" mathematical values

**This is expected behavior** - the protocol accepts these small rounding discrepancies. Our code must match this tolerance.

**Related to Issue #0012:**
This is the same "interest exceeds withdrawal" scenario documented in Issue #0012. The difference is that #0012 addressed the case where `amount == balance_increase`, while this issue addresses the case where `amount < balance_increase` but with a rounding error.

## Refactoring

1. **Add Rounding Tolerance Constants:** Define explicit tolerance values for different types of comparisons:
   - `RAY_MATH_TOLERANCE = 2` for ray math operations
   - `POOL_REVISION_TOLERANCE = {9: 2, ...}` for revision-specific tolerances

2. **Create Helper Function:** Extract tolerance comparison into a reusable function:
   ```python
   def within_tolerance(actual: int, expected: int, tolerance: int = 1) -> bool:
       return abs(actual - expected) <= tolerance
   ```

3. **Audit Other Comparisons:** Review all exact equality checks involving:
   - `balance_increase - amount`
   - Ray math calculations
   - Pool revision 9+ pre-scaled amounts

4. **Document Rounding Behavior:** Add comments explaining why tolerances are needed:
   - Reference Aave's ray math implementation
   - Explain round-half-up behavior
   - Document expected ±1-2 wei variance

## Related Issues

- **Issue #0012:** V4 Withdraw Emits Mint When Interest Exceeds Withdrawal - Original handling for this scenario
- **Issue #0004:** Interest Accrual Scaling Error in Enrichment - Related to Mint event processing
- **AToken.sol:** `_burnScaled` function documentation on mint-during-withdrawal behavior

## Files Involved

- `src/degenbot/cli/aave_transaction_operations.py` - Lines 1108-1112 (interest mint matching)
- `contract_reference/aave/AToken/rev_4.sol` - Lines 2836-2850 (_burnScaled function)
- `contract_reference/aave/libraries/TokenMath.sol` - Ray math rounding implementation

## Verification Steps

After implementing fix:
```bash
# Run specific block
uv run degenbot aave update --chunk 1

# Verify balance at block 23094514
uv run python3 -c "
from web3 import Web3
w3 = Web3(Web3.HTTPProvider('http://node:8545'))
result = w3.eth.call({
    'to': '0x4F5923Fc5FD4a93352581b38B7cD26943012DECF',
    'data': '0x1da8d368000000000000000000000000ef9bb2F993631D4a8E36548Ab722B081087176b8',
}, block_identifier=23094514)
print('Scaled balance:', int(result.hex(), 16))
"
```

## Notes

- This rounding behavior is inherent to Aave V3's ray math implementation
- Similar issues may occur in other scenarios involving chained ray operations
- The ±1 wei tolerance should be sufficient for all normal operations
- Consider ±2 tolerance for pool revision 9+ which pre-scales amounts before token calls
