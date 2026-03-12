# Issue: V4 Debt Burn Rounding Error

## Status
**RESOLVED** - Fix implemented and verified

## Date
2026-03-12

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), underlying_token=Erc20TokenTable(chain=1, address='0x514910771AF9Ca656af840dff83E8264EcF986CA', symbol=None), a_token=Erc20TokenTable(chain=1, address='0x5E8C8A7243651DB1384C0dDfDbE39761E8e7E51a', symbol=None), v_token=Erc20TokenTable(chain=1, address='0x4228F8895C7dDA20227F6a5c6751b8Ebf19a6ba8', symbol=None)). User AaveV3User(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), address='0x21e7824340C276735a033b1bC45652EbBe007193', e_mode=0) scaled balance (29410404374552234237337) does not match contract balance (29410404374552234237336) at block 23088593
```

## Root Cause
The V4 debt token processor uses a fallback calculation when `scaled_delta` is not provided, which introduces rounding errors. The processor calculates the scaled burn amount from the Burn event's `value` and `balance_increase` fields using `ray_div_floor`, but this reverse calculation can be off by 1 wei compared to the contract's original calculation.

### Contract Flow
1. Pool.executeRepay() calculates: `scaledAmount = paybackAmount.getVTokenBurnScaledAmount(index)` using `rayDivFloor`
2. vToken.burn() receives this pre-calculated `scaledAmount` and burns it
3. vToken._burnScaled() calculates: `amountToBurn = previousBalance - nextBalance` using `getVTokenBalance()` which uses `rayMulCeil`
4. The Burn event emits: `Burn(from, target, amountToBurn, balanceIncrease, index)`

### The Problem
The Python code receives the Burn event and tries to reverse-calculate the scaled amount:
```python
requested_amount = event_data.value + event_data.balance_increase
scaled_delta = ray_div_floor(requested_amount, index)
```

While mathematically `ray_div_floor(ray_mul_ceil(x), index)` should equal `x`, rounding in the intermediate steps can cause a 1 wei discrepancy.

### Debug Values
- **Transaction**: 0x121166f6d925e38e425a6dfa637a71cfa3bc6ed2d08653cf2aad146d2a6077c3
- **Block**: 23088593
- **Asset**: LINK Variable Debt Token (0x4228F8895C7dDA20227F6a5c6751b8Ebf19a6ba8)
- **User**: 0x21e7824340C276735a033b1bC45652EbBe007193
- **Operation**: INTEREST_ACCRUAL (debt burn during repay)
- **logIndex**: 130
- **vToken Revision**: 4

**Event Values:**
- value (amountToBurn): 499997055676410400534
- balance_increase: 2944323589599465
- index: 1009133546217410998733439284

**Calculated:**
- requested_amount = 499997055676410400534 + 2944323589599465 = 500000000000000000000 (500 LINK)
- scaled_delta = ray_div_floor(500e18, index) = 495474560204817907241

**Balance Change:**
- Initial scaled balance: 29905878934757052144577
- Burn scaled amount: 495474560204817907241
- Expected final scaled balance: 29410404374552234237336
- Contract scaled balance: 29410404374552234237336
- Python calculated balance: 29410404374552234237337 (off by 1 wei)

## Transaction Details
| Field | Value |
|-------|-------|
| Hash | 0x121166f6d925e38e425a6dfa637a71cfa3bc6ed2d08653cf2aad146d2a6077c3 |
| Block | 23088593 |
| Type | REPAY + WITHDRAW |
| User | 0x21e7824340C276735a033b1bC45652EbBe007193 |
| Asset | LINK (0x514910771AF9Ca656af840dff83E8264EcF986CA) |
| vToken | 0x4228F8895C7dDA20227F6a5c6751b8Ebf19a6ba8 |
| Pool | 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 |
| vToken Revision | 4 |

## Fix Applied

**Files Modified**:
1. `src/degenbot/cli/aave_types.py` - Added `last_repay_amount` field to TransactionContext
2. `src/degenbot/cli/aave_event_matching.py` - Updated to pass scaled_event to matcher and provide raw_amount for debt burns
3. `src/degenbot/cli/aave.py` - Pre-process REPAY operations, use stored paybackAmount for INTEREST_ACCRUAL debt burns, calculate scaled_amount for V4+ tokens

**Root Cause**: The code had three issues:
1. INTEREST_ACCRUAL operations didn't have access to the REPAY event's paybackAmount
2. For INTEREST_ACCRUAL debt burns, the code was reverse-calculating paybackAmount from Burn event values (amount + balance_increase), which differs from the actual paybackAmount by 1 wei due to rounding
3. For non-GHO V4+ debt burns, scaled_amount was never calculated from raw_amount

**Solution**: 

1. **Pre-process REPAY operations** to extract and store paybackAmount in TransactionContext before processing scaled token events:
```python
# Pre-process REPAY operations to extract paybackAmounts
for operation in sorted_operations:
    if operation.operation_type in {REPAY, REPAY_WITH_ATOKENS, GHO_REPAY}:
        if operation.pool_event is not None:
            decoded = eth_abi.abi.decode(["uint256", "bool"], operation.pool_event["data"])
            tx_context.last_repay_amount = decoded[0]
```

2. **Use stored paybackAmount** for INTEREST_ACCRUAL debt burns:
```python
if operation.operation_type == OperationType.INTEREST_ACCRUAL:
    if tx_context.last_repay_amount > 0:
        raw_amount = tx_context.last_repay_amount
```

3. **Calculate scaled_amount** for V4+ debt burns using TokenMath:
```python
if raw_amount is not None and debt_asset.v_token_revision >= 4:
    token_math = TokenMathFactory.get_token_math_for_token_revision(
        debt_asset.v_token_revision
    )
    scaled_amount = token_math.get_debt_burn_scaled_amount(
        amount=raw_amount,
        borrow_index=scaled_event.index,
    )
```

**Verification**: The fix successfully processes blocks 23088593-23088594 without balance verification errors.

## Key Insight
Aave v3.5+ uses asymmetric rounding to protect the protocol:
- `rayDivFloor` when calculating scaled amounts to burn (prevents over-burning)
- `rayMulCeil` when calculating actual balances (prevents under-accounting debt)

When reverse-calculating the scaled amount from event values, this asymmetry can cause 1 wei discrepancies. The solution is to use the original `paybackAmount` from the REPAY event to calculate the scaled amount, matching the contract's calculation exactly.

## Refactoring
1. **TokenMath Integration**: Ensure all V4+ debt burn operations extract the `raw_amount` from REPAY events and use `TokenMath.get_debt_burn_scaled_amount()` to calculate the exact scaled amount.

2. **Processor Interface**: The `DebtBurnEvent` should include a `scaled_amount` field that is populated from event parsing or REPAY event extraction, eliminating the need for reverse calculation.

3. **Verification Tolerance**: Consider implementing a 1 wei tolerance in the balance verification for debt tokens, acknowledging that rounding differences between rayDivFloor/rayMulCeil operations can produce this expected behavior.

4. **Event Enrichment**: When parsing scaled token events, enrich them with pre-calculated scaled amounts from the triggering pool event (REPAY, BORROW, etc.) to avoid recalculation.
