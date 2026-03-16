## Issue: V4 REPAY Missing Burn Event Handling

## Date: 2026-03-15

## Symptom

```
AssertionError: Balance verification failure for AaveV3Asset(...USDT...). 
User AaveV3User(...0xf388D96F92e1035AcF7AfD8173b87482Cca8992F...) 
scaled balance (16244804928260) does not match contract balance (16244804928259) 
at block 23089241
```

**Off-by-one error:** Calculated balance is 1 wei higher than actual contract balance.

## Root Cause

### Transaction Analysis

**Transaction:** `0x481d89243dd0e31322e87a8e9cdbeaa96e62f3a58c903bcafa1576d0cc0258f9`
**Block:** 23089241
**Type:** REPAY (USDT variable debt)
**User:** `0xf388D96F92e1035AcF7AfD8173b87482Cca8992F`
**Amount:** 673,147,659 USDT (6 decimals)

### Events in Transaction

| Log Index | Event Type | Contract | Details |
|-----------|------------|----------|---------|
| 443 | Transfer | VariableDebtToken | Interest accrual mint: 1,575,541,619 scaled |
| 444 | Mint | VariableDebtToken | Debt mint: amount=1,575,541,619, balanceIncrease=2,248,689,277 |
| 445 | ReserveDataUpdated | Pool | Variable borrow index updated |
| 446 | Transfer | USDT | Repayment: 673,147,659 USDT to aToken |
| 447 | Repay | Pool | REPAY event |

**Critical Finding:** A **Mint event** (logIndex 444) is emitted instead of a Burn event because accrued interest (2.25 USDT) exceeds the repayment amount (0.67 USDT).

### Contract Behavior

The Aave V3 Pool contract (revision 9) handles repayments via `executeRepay()`:

```solidity
// 1. Update state and accrue interest
reserve.updateState(reserveCache);

// 2. Calculate user debt with accrued interest
uint256 userDebtScaled = IVariableDebtToken(reserveCache.variableDebtTokenAddress)
    .scaledBalanceOf(params.onBehalfOf);
uint256 userDebt = userDebtScaled.getVTokenBalance(reserveCache.nextVariableBorrowIndex);

// 3. Determine payback amount
uint256 paybackAmount = params.amount;
if (paybackAmount > userDebt) {
    paybackAmount = userDebt;
}

// 4. Burn debt tokens
IVariableDebtToken(reserveCache.variableDebtTokenAddress).burn({
    from: params.onBehalfOf,
    scaledAmount: paybackAmount.getVTokenBurnScaledAmount(reserveCache.nextVariableBorrowIndex),
    index: reserveCache.nextVariableBorrowIndex
});
```

### The Problem

1. **Interest accrues first** during `updateState()` - the user's cached index is updated
2. **Debt is burned** during the repayment - scaled tokens are reduced by `paybackAmount.getVTokenBurnScaledAmount(index)` = 568,978,877
3. **A Mint event is emitted** because the unscaled debt increased net (interest 2.25 > repayment 0.67), even though scaled tokens were burned

The VariableDebtToken (revision 4) emits either a Burn or Mint event depending on the net effect on unscaled debt balance, but scaled tokens are always burned during repayment.

### Code Path Analysis

In `aave_transaction_operations.py:_create_standard_repay_operation()`:

```python
def _create_standard_repay_operation(...):
    # Find principal debt burn
    debt_burn_event = self._find_matching_debt_burn(...)
    
    if debt_burn_event is not None:
        scaled_token_events.append(debt_burn_event)
    
    # Find interest accrual debt burn
    interest_burn_event = self._find_interest_accrual_debt_burn(...)
    
    if interest_burn_event is not None:
        scaled_token_events.append(interest_burn_event)
    
    # If no burns found, create minimal operation
    if not scaled_token_events:
        logger.debug(
            f"REPAY at logIndex={repay_log_index} has no matching burn events, "
            f"creating minimal operation"
        )
        return Operation(..., scaled_token_events=[], ...)
```

When no burn events are found, the operation is created with **empty scaled_token_events**. This means:
1. The interest accrual Mint event is not processed as part of the REPAY operation
2. The repayment burn is not accounted for at all
3. The balance verification fails because the actual burn is not reflected in our calculated balance

### Mathematical Impact

```
Starting scaled balance:          16,245,373,907,136
Actual burn (scaled):                      568,978,877  ← Not matched!
------------------------------------------------------------------
Expected final balance:           16,244,804,928,259
Actual final balance (contract):  16,244,804,928,259  ✓
Calculated balance (before fix):  16,244,804,928,260  ← 1 wei too high
```

The 1 wei discrepancy occurred because:
- The Mint event at logIndex 444 represents the NET unscaled change (interest - repayment)
- We incorrectly processed it as a mint (+1,575,541,619 scaled) instead of a burn (-568,978,877 scaled)
- The scaled burn amount must be calculated from the Pool event using `TokenMath.get_debt_burn_scaled_amount()`

## Transaction Details

| Field | Value |
|-------|-------|
| **Hash** | `0x481d89243dd0e31322e87a8e9cdbeaa96e62f3a58c903bcafa1576d0cc0258f9` |
| **Block** | 23089241 |
| **Type** | REPAY (variable debt) |
| **User** | `0xf388D96F92e1035AcF7AfD8173b87482Cca8992F` |
| **Asset** | USDT (`0xdAC17F958D2ee523a2206206994597C13D831ec7`) |
| **Amount** | 673,147,659 (673.15 USDT) |
| **vToken** | `0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8` |
| **vToken Implementation** | `0x2b31caa35900f4c8fe6151ccaf8d0ea4a89743a1` |
| **vToken Revision** | 4 |
| **Pool Revision** | 9 |

## Fix

### Implementation

**Files Modified:**
1. `src/degenbot/cli/aave_transaction_operations.py`
2. `src/degenbot/cli/aave.py`

### Cleanup: Removed Dead Code

**Function removed:** `_find_interest_accrual_debt_burn()`

This function searched for `DEBT_INTEREST_BURN` events that don't exist in the VariableDebtToken contract. The VariableDebtToken only emits:
- `Burn` events (when repayment > interest)
- `Mint` events (when interest > repayment)

Interest accrual is stored in the `balance_increase` field of these events, not as separate events. The function always returned `None` and was therefore dead code.

### Changes

#### 1. Event Matching (`aave_transaction_operations.py`)

Renamed `_find_matching_debt_burn()` to `_find_principal_repay_event()` and modified to match both Burn and Mint events:

```python
def _find_principal_repay_event(...):
    """Find the principal debt event (Burn or Mint) associated with a REPAY.
    
    For REPAY operations, the VariableDebtToken emits either:
    - Burn event: when repayment > interest (net decrease)
    - Mint event: when interest > repayment (net increase)
    """
    
    valid_event_types = (
        {ScaledTokenEventType.GHO_DEBT_BURN, ScaledTokenEventType.GHO_DEBT_MINT}
        if is_gho
        else {ScaledTokenEventType.DEBT_BURN, ScaledTokenEventType.DEBT_MINT}
    )
    
    # For DEBT_BURN: calculated_amount = amount + balance_increase
    # For DEBT_MINT: calculated_amount = balance_increase - amount
    if ev.event_type in {DEBT_BURN, GHO_DEBT_BURN}:
        calculated_amount = ev.amount + ev.balance_increase
    else:  # DEBT_MINT
        calculated_amount = ev.balance_increase - ev.amount
```

**Key fix:** Removed the check `if ev.target_address != ZERO_ADDRESS: continue` for Mint events, as Mint events have `target_address=None`.

#### 2. Event Processing (`aave.py`)

Modified `_process_debt_mint_with_match()` to detect REPAY context and treat Mint events as burns:

```python
def _process_debt_mint_with_match(...):
    if operation.operation_type in {OperationType.REPAY, OperationType.GHO_REPAY}:
        # Treat as burn: calculate actual scaled burn from Pool event
        repay_amount, _ = eth_abi.abi.decode(
            types=["uint256", "bool"],
            data=operation.pool_event["data"],
        )
        # Use TokenMath to match on-chain calculation
        token_math = TokenMathFactory.get_token_math(operation.pool_revision)
        actual_scaled_burn = token_math.get_debt_burn_scaled_amount(
            repay_amount, scaled_event.index
        )
        
        _process_scaled_token_operation(
            event=DebtBurnEvent(
                ...,
                scaled_amount=actual_scaled_burn,  # Critical: use calculated burn
            ),
            ...
        )
```

**Note:** Using `TokenMath.get_debt_burn_scaled_amount()` instead of manual ray division ensures version-specific rounding behavior is handled correctly.

### Root Cause of Original Failure

The processor was calculating the scaled burn amount incorrectly:
- **Before:** Using `enriched_event.scaled_amount` which was derived from Mint event data
- **After:** Using `TokenMath.get_debt_burn_scaled_amount(repay_amount, index)` from Pool event

The Mint event's `amount` field represents the net unscaled increase (interest - repayment), not the scaled burn amount. The contract burns exactly `paybackAmount.getVTokenBurnScaledAmount(index)` scaled tokens regardless of which event is emitted.

## Key Insight

**The VariableDebtToken emits either a Burn OR Mint event for every repayment - never neither.** The contract determines which event to emit based on whether the repayment amount exceeds the accrued interest:

```solidity
// In _burnScaled() (VariableDebtToken rev_4)
if (nextBalance > previousBalance) {
    // Interest > repayment (net increase)
    emit Mint(user, user, amountToMint, balanceIncrease, index);
} else {
    // Repayment > interest (net decrease)
    emit Burn(user, target, amountToBurn, balanceIncrease, index);
}
```

**The Real Problem:** Our matching logic only searched for `DEBT_BURN` events, but this transaction emitted a `DEBT_MINT` event (because accrued interest 2.25 USDT > repayment 0.67 USDT). The scaled tokens were still burned - we just weren't matching the event that proved it.

**Lesson:** When the same operation can emit different event types depending on runtime conditions (interest rates, repayment amounts), matching logic must account for all possible event types. The Pool events always have corresponding scaled token events - we just need to match the right one.

## Refactoring

1. **Unify event matching for dual-event scenarios** - Create a generalized pattern for operations that can emit different event types (Burn vs Mint) based on runtime conditions. Apply this pattern consistently across REPAY, BORROW, and other operations.

2. **Document event emission patterns** - Create a reference table showing which operations emit which events under which conditions for each contract revision. This helps future debugging when events don't match expectations.

3. **Add validation for event matching** - After operation creation, verify that each REPAY operation has exactly one principal debt event (Burn or Mint) matched. Log warnings if events are found but not matched.

4. **Extract TokenMath usage patterns** - Ensure all scaled amount calculations use TokenMath consistently rather than manual calculations. Add linting rules or type checking to prevent manual ray division.

5. **Enhance test coverage** - Add test cases for REPAY operations where interest > repayment (Mint event) and interest < repayment (Burn event) to ensure both paths work correctly.

## Related Issues

- Issue #0001: V4 Debt Burn Rounding Error (similar off-by-one errors)
- Issue #0002: V4 Collateral Burn Rounding Error (rounding behavior)
- Issue #0007: Interest Accrual Burn Amount Zeroed (interest accrual handling)

---

**Filename:** 0010 - V4 REPAY Missing Burn Event Handling.md
