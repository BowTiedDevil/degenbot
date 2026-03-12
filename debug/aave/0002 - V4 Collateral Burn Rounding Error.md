# Issue: V4 Collateral Burn Rounding Error (INTEREST_ACCRUAL)

## Status
**RESOLVED** - Fix implemented and verified

## Date
2026-03-12

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), underlying_token=Erc20TokenTable(chain=1, address='0xdAC17F958D2ee523a2206206994597C13D831ec7', symbol=None), a_token=Erc20TokenTable(chain=1, address='0x23878914EFE38d27C4D67Ab83ed1b93A74D4086a', symbol=None), v_token=Erc20TokenTable(chain=1, address='0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8', symbol=None)). User AaveV3User(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), address='0x7Bc3485026Ac48b6cf9BaF0A377477Fff5703Af8', e_mode=0) scaled balance (66837697650187) does not match contract balance (66837697650188) at block 23088622
```

## Root Cause
The Python code reverse-calculates the scaled burn amount from the CollateralBurn event's `value` and `balance_increase` fields for INTEREST_ACCRUAL operations. Due to floor operations in the contract's underlying balance calculations, `value + balance_increase` (266,999,198,478) differs from the actual withdraw amount (266,999,198,477) by 1 wei, causing a 1 wei discrepancy in the scaled amount.

### Contract Flow
1. Pool.executeWithdraw() calculates: `scaledAmount = amount.rayDivCeil(index)` 
2. aToken.burn() receives this pre-calculated `scaledAmount` and burns it
3. aToken._burnScaled() calculates: `amountToBurn = previousBalance - nextBalance` using `rayMulFloor`
4. The Burn event emits: `Burn(from, target, amountToBurn, balanceIncrease, index)`

### The Problem
The Python code receives the CollateralBurn event and tries to reverse-calculate the scaled amount:
```python
requested_amount = event_data.value + event_data.balance_increase
scaled_delta = ray_div_ceil(requested_amount, index)
```

Due to floor operations in `rayMulFloor`, mathematically `floor(a) - floor(b) != floor(a - b)`, causing `value + balance_increase` to differ from the actual withdraw amount by 1 wei.

### Debug Values
- **Transaction**: 0xe897f544fe6068bd22bea5c82a6d5e09c0803b364470851be8f27ac76ab377bf
- **Block**: 23088622
- **Asset**: USDT aToken (0x23878914EFE38d27C4D67Ab83ed1b93A74D4086a)
- **User**: 0x7Bc3485026Ac48b6cf9BaF0A377477Fff5703Af8
- **Operation**: INTEREST_ACCRUAL (collateral burn during withdraw)
- **logIndex**: 52
- **aToken Revision**: 4

**Event Values:**
- value (amountToBurn): 266,936,161,923
- balance_increase: 63,036,555
- index: 1132456706666613700181208272

**Calculated:**
- requested_amount = 266,936,161,923 + 63,036,555 = 266,999,198,478
- scaled_delta = ray_div_ceil(266999198478, index) = 235,769,894,696

**Actual:**
- Pool withdraw amount: 266,999,198,477 (1 wei less!)
- Contract scaled burn: 235,769,894,695
- Python calculated burn: 235,769,894,696 (off by 1 wei)

**Balance Change:**
- Initial scaled balance: 67,073,467,544,883
- Burn scaled amount: 235,769,894,695
- Expected final scaled balance: 66,837,697,650,188
- Contract scaled balance: 66,837,697,650,188
- Python calculated balance: 66,837,697,650,187 (off by 1 wei)

## Transaction Details
| Field | Value |
|-------|-------|
| Hash | 0xe897f544fe6068bd22bea5c82a6d5e09c0803b364470851be8f27ac76ab377bf |
| Block | 23088622 |
| Type | WITHDRAW (via Balancer V3) |
| User | 0x7Bc3485026Ac48b6cf9BaF0A377477Fff5703Af8 |
| Asset | USDT (0xdAC17F958D2ee523a2206206994597C13D831ec7) |
| aToken | 0x23878914EFE38d27C4D67Ab83ed1b93A74D4086a |
| Pool | 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 |
| aToken Revision | 4 |

## Fix Applied

**Files Modified**:
1. `src/degenbot/cli/aave_types.py` - Added `last_withdraw_amount` field to TransactionContext
2. `src/degenbot/cli/aave.py` - Pre-process WITHDRAW operations and use stored amount for INTEREST_ACCRUAL burns

**Root Cause**: The code reverse-calculates the scaled burn amount from Burn event values, which differs from the actual withdraw amount by 1 wei due to rounding in the contract's floor operations.

**Solution** (following Issue #0001 pattern):

1. **Add field to TransactionContext**:
```python
# In aave_types.py
last_withdraw_amount: int = 0
```

2. **Pre-process WITHDRAW operations** to extract and store withdraw amounts:
```python
# In aave.py _process_transaction()
for operation in sorted_operations:
    elif (
        operation.operation_type == OperationType.WITHDRAW
        and operation.pool_event is not None
        and operation.pool_event.get("data")
    ):
        decoded = eth_abi.abi.decode(["uint256"], operation.pool_event["data"])
        tx_context.last_withdraw_amount = decoded[0]
```

3. **Use stored withdraw amount** for INTEREST_ACCRUAL collateral burns:
```python
# In _process_collateral_burn_with_match()
if operation is not None and operation.operation_type == OperationType.INTEREST_ACCRUAL:
    if tx_context.last_withdraw_amount > 0:
        raw_amount = tx_context.last_withdraw_amount
        logger.debug(f"Using stored WITHDRAW amount for INTEREST_ACCRUAL: {raw_amount}")
```

4. **Calculate scaled_amount** using TokenMath (existing code at lines 2673-2684 handles this):
```python
if scaled_amount is None and raw_amount is not None and collateral_asset.a_token_revision >= 4:
    token_math = TokenMathFactory.get_token_math_for_token_revision(
        collateral_asset.a_token_revision
    )
    scaled_amount = token_math.get_collateral_burn_scaled_amount(
        amount=raw_amount,
        liquidity_index=scaled_event.index,
    )
```

**Verification**: The fix successfully processes blocks 23088622-23088623 without balance verification errors.

## Key Insight
Aave v3.4+ uses asymmetric rounding to protect the protocol:
- `rayDivCeil` when calculating scaled amounts to burn (prevents over-burning)
- `rayMulFloor` when calculating actual balances (prevents under-accounting collateral)

When reverse-calculating the scaled amount from event values, this asymmetry can cause 1 wei discrepancies. The solution is to use the original `withdrawAmount` from the Pool event to calculate the scaled amount, matching the contract's calculation exactly.

This is the same pattern as Issue #0001 (debt token burns) but applied to collateral token burns.

## Refactoring
1. **Consistent Pattern**: Apply the same pre-processing pattern for all Pool events (SUPPLY, WITHDRAW, BORROW, REPAY) to extract raw_amounts and avoid reverse-calculation rounding errors.

2. **Event Enrichment**: When parsing INTEREST_ACCRUAL operations, enrich them with pre-calculated scaled amounts from the triggering Pool event to avoid recalculation.

3. **Verification Tolerance**: Consider implementing a 1 wei tolerance in balance verification for interest accrual operations, acknowledging that rounding differences between rayDivCeil/rayMulFloor operations can produce this expected behavior.

4. **TokenMath Integration**: Ensure all V4+ collateral burn operations (both WITHDRAW and INTEREST_ACCRUAL) use `TokenMath.get_collateral_burn_scaled_amount()` to calculate the exact scaled amount from the Pool event's raw amount.
