# 0044 - Pool Revision 9 Liquidation Burn Amount Calculation Error

## Issue
Balance verification failure for user with vUSDT debt position at block 23549932.

## Date
2026-03-20

## Status
✅ **RESOLVED**

## Resolution
Fixed by calculating the scaled burn amount using `debtToCover.rayDivFloor(index)` instead of using the Burn event's `amount` field directly.

## Testing
- Ran `uv run degenbot aave update` successfully
- Verified processing completes without balance verification errors
- Confirmed balances match on-chain state at block 23,549,933

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...). User AaveV3User(...) scaled balance (7410624974) does not match contract balance (9217164623) at block 23549932
```

**Key Data Points:**
- User: `0x0FA2012F2F02E005472502C7A64C0371CC6d7E74`
- Asset: USDT (vToken: 0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8)
- Calculated balance: 7,410,624,974
- On-chain balance: 9,217,164,623
- Difference: 1,806,539,649 (significant mismatch)

## Root Cause

For **Pool Revision 9+ liquidations**, the code was using the wrong amount for the scaled burn calculation.

### The Misunderstanding

**Original (incorrect) assumption:** The Burn event's `amount` field is the scaled burn amount.

**Reality:** The Burn event's `amount` field is the calculated current debt value in **underlying units**, not scaled units.

### Contract Behavior

**Pool Revision 9 (rev_9.sol) `_burnDebtTokens` function:**
```solidity
uint256 burnAmount = hasNoCollateralLeft ? borrowerReserveDebt : actualDebtToLiquidate;

// As vDebt.burn rounds down, we ensure an equivalent of <= amount debt is burned.
(noMoreDebt, debtReserveCache.nextScaledVariableDebt) = IVariableDebtToken(
  debtReserveCache.variableDebtTokenAddress
).burn({
    from: borrower,
    scaledAmount: burnAmount.getVTokenBurnScaledAmount(
      debtReserveCache.nextVariableBorrowIndex
    ),
    index: debtReserveCache.nextVariableBorrowIndex
  });
```

**VariableDebtToken Revision 4 (rev_4.sol) `_burnScaled` function:**
```solidity
uint256 scaledBalance = super.balanceOf(user);
uint256 nextBalance = getTokenBalance(scaledBalance - amountScaled, index);
uint256 previousBalance = getTokenBalance(scaledBalance, _userState[user].additionalData);
uint256 balanceIncrease = getTokenBalance(scaledBalance, index) - previousBalance;

// ...

uint256 amountToBurn = previousBalance - nextBalance;
emit Transfer(user, target, amountToBurn);
emit Burn(user, target, amountToBurn, balanceIncrease, index);
```

**Key Formulas:**
- `scaledAmount = debtToCover.rayDivFloor(index)` (actual burn)
- `amountToBurn = scaledAmount.rayMul(index) + balanceIncrease` (Burn event amount)

### Transaction Data

**Block:** 23549932
**Transaction:** `0x537160087e7eb8cac9e7c57f72e7637ba7468409ee1f4dc139fb4d46bf1307f9`
**User:** `0x0FA2012F2F02E005472502C7A64C0371CC6d7E74`

| Field | Value |
|-------|-------|
| `debtToCover` | 11,026,542,078 |
| `index` | 1,196,317,865,990,294,248,781,143,542 |
| `balance_increase` | 2,935,300 |
| **Calculated scaled burn** | **9,217,067,128** ✓ |
| Burn event `amount` | 11,023,606,777 (underlying units) |

**Verification:**
```
Balance at block 23549931: 18,434,231,751
Balance at block 23549932: 9,217,164,623
Actual change: 9,217,067,128 ✓ (matches calculated scaled burn)
```

### The Bug

The code was using `scaled_event.amount` (11,023,606,777) which is the Burn event amount in underlying units, but should have been calculating the scaled burn amount using `debtToCover.rayDivFloor(index)` = 9,217,067,128.

## Transaction Details

- **Hash:** `0x537160087e7eb8cac9e7c57f72e7637ba7468409ee1f4dc139fb4d46bf1307f9`
- **Block:** 23549932
- **Type:** LIQUIDATION
- **User Liquidated:** `0x0FA2012F2F02E005472502C7A64C0371CC6d7E74`
- **Debt Asset:** USDT (vToken: 0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8)
- **Collateral Asset:** WETH (aToken: 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8)
- **Pool Revision:** 9
- **Token Revisions:** aToken=4, vToken=4

### Events for User 0x0FA201...

| LogIndex | Token | Event Type | Amount | Notes |
|----------|-------|------------|--------|-------|
| 221 | vUSDT | Transfer | 11,023,606,777 | Debt transfer to zero address |
| 222 | vUSDT | Burn | 11,023,606,777 | **Burn event amount (underlying units)** |
| 226 | aWETH | Burn | 3,006,226,578,130,523,025 | Collateral burn |
| 228 | aWETH | BalanceTransfer | 35,652,109,251,347,217 | Liquidation fee to treasury |
| 231 | Pool | LiquidationCall | debtToCover=11,026,542,078 | Pool event |

## Fix

**Files:** 
- `src/degenbot/aave/enrichment.py` (lines 139-161)
- `src/degenbot/cli/aave.py` (lines 3533-3548)

### Enrichment Layer Fix

Calculate the scaled amount from `debtToCover` using `rayDivFloor` with the index from the burn event:

```python
# Pool Revision 9+ passes pre-scaled amounts to token contracts
# The Pool calculates scaledAmount = debtToCover.rayDivFloor(index)
# and passes it to vToken.burn(). We must calculate this ourselves.
# See debug/aave/0044 for details
if self.pool_revision >= 9:  # noqa: PLR2004
    # Calculate scaled amount from debtToCover using the index from the burn event
    # scaledAmount = debtToCover / index (floor division)
    assert scaled_event.index is not None
    calculator = ScaledAmountCalculator(
        pool_revision=self.pool_revision,
        token_revision=token_revision,
    )
    scaled_amount = calculator.calculate(
        event_type=ScaledTokenEventType.DEBT_BURN,
        raw_amount=raw_amount,
        index=scaled_event.index,
    )
    logger.debug(
        f"ENRICHMENT: Pool Rev {self.pool_revision} LIQUIDATION "
        f"calculated scaled amount: {scaled_amount} "
        f"from debtToCover={raw_amount} / index={scaled_event.index}"
    )
    calculation_event_type = scaled_event.event_type
    # Skip to event creation
    return self._create_enriched_event(...)
```

### Processing Layer

No changes needed - already uses `enriched_event.scaled_amount`:

```python
if operation and operation.operation_type in LIQUIDATION_OPERATION_TYPES:
    if tx_context.pool_revision >= 9:  # noqa: PLR2004
        burn_value = enriched_event.scaled_amount or enriched_event.raw_amount
        logger.debug(
            f"_process_debt_burn_with_match: NORMAL LIQUIDATION (Pool Rev 9+) - using "
            f"scaled_amount={burn_value}"
        )
```

### Why This Fix is Correct

1. **Contract behavior:** The Pool calls `vToken.burn(scaledAmount, index)` where `scaledAmount = debtToCover.rayDivFloor(index)`
2. **TokenMath alignment:** Uses the same `rayDivFloor` calculation as the contract's `getVTokenBurnScaledAmount`
3. **Burn event semantics:** The Burn event's `amount` field is the calculated current debt value, not the scaled burn
4. **Separation of concerns:** Enrichment calculates the correct amount, processing uses it directly

## Key Insight

**Pool Revision 9+ liquidation debt burn formula:**

```
scaled_burn_amount = debtToCover.rayDivFloor(index)
```

Where:
- `debtToCover`: Total debt to be liquidated in underlying units (from LiquidationCall event)
- `index`: Current borrow index (from Burn event)
- `scaled_burn_amount`: Actual reduction in scaled debt balance

**Burn event amount ≠ scaled burn amount:**
- Burn event `amount` = `scaled_burn_amount.rayMul(index) + balanceIncrease` (current debt value)
- This is used for display/tracking purposes, not for balance calculations
- The actual balance reduction is always the scaled amount

## Refactoring Recommendations

1. **Centralize amount calculation logic:**
   - Create a `LiquidationAmountCalculator` class that handles all revision-specific calculations
   - Include proper handling for index-based scaling

2. **Add validation:**
   - Verify that calculated burn amount produces expected balance change
   - Log warnings when calculations differ significantly from Burn event amount

3. **Update documentation:**
   - Document the difference between Burn event amount and scaled burn amount
   - Explain that Burn event `amount` is for display, scaled amount is for balance updates

4. **Consider test coverage:**
   - Add test case for liquidation with accrued interest
   - Verify both Pool Rev 8 and Rev 9 behavior

## Verification

After implementing the fix:
1. Enrichment extracts `debtToCover = 11,026,542,078` and `index = 1,196,317,865,990,294,248,781,143,542`
2. Calculates `scaled_amount = debtToCover.rayDivFloor(index) = 9,217,067,128`
3. Processing applies burn of 9,217,067,128
4. Final user balance: 9,217,164,623
5. On-chain balance: 9,217,164,623
6. Match: ✅

## Related Issues

- Issue 0042: Pool Revision 9 Liquidation Debt Amount Already Scaled
  - Correctly identified that Pool Rev 9+ uses pre-scaled amounts
  - But the initial fix used the wrong source (Burn event amount instead of calculated amount)
- Issue 0043: Multi-Liquidation Secondary Debt Burn Misclassification
  - Discovered in same block, different root cause

Both issues were discovered while investigating block 23549932.

## Impact Assessment

**Affected Users:** Any user with debt positions liquidated on Pool Revision 9+

**Affected Blocks:** All blocks >= deployment of Pool Rev 9 where liquidations occurred

**Data Quality Impact:** 
- Historical debt balances were incorrect for liquidated positions
- The error compounded over multiple liquidations for the same user
- Collateral positions were not affected (only debt positions)

**Risk Level:** High (affects core accounting functionality)

## Fix Details

**Commit:** Changes to `src/degenbot/aave/enrichment.py`

**Before:**
```python
if self.pool_revision >= 9:
    scaled_amount = scaled_event.amount  # WRONG: underlying units
```

**After:**
```python
if self.pool_revision >= 9:
    calculator = ScaledAmountCalculator(...)
    scaled_amount = calculator.calculate(
        event_type=ScaledTokenEventType.DEBT_BURN,
        raw_amount=raw_amount,
        index=scaled_event.index,
    )  # CORRECT: scaled units via rayDivFloor
```

## Contract References

**Pool Revision 9 (rev_9.sol):**
- `liquidationCall()` calculates debt and calls `_burnDebtTokens()`
- `_burnDebtTokens()` calculates `scaledAmount = debtToCover.getVTokenBurnScaledAmount(index)`
- See `_burnDebtTokens` function around line 9933

**VariableDebtToken Revision 4 (rev_4.sol):**
- `burn(scaledAmount, index)` receives pre-calculated scaled amount
- `_burnScaled()` emits Burn event with calculated current debt value
- `getVTokenBurnScaledAmount(amount, index)` = `amount.rayDivFloor(index)`
- See `burn()` function around line 184, `_burnScaled()` around line 3908

## Lessons Learned

1. **Event field semantics matter:** The `amount` field in Burn events represents different things depending on context
2. **Always verify units:** Scaled vs underlying units must be explicitly tracked
3. **Follow the contract code:** The Pool contract's calculation logic is the source of truth
4. **Test with real data:** Balance verification caught this issue immediately
