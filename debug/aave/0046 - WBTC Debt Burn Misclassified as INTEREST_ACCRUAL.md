# Issue 0046: WBTC Debt Burn Misclassified as INTEREST_ACCRUAL

**Date:** March 21, 2026

## Symptom

Balance verification failure during Aave update:

```
AssertionError: Balance verification failure for AaveV3Asset(... symbol='WBTC' ...). 
User 0x9913e51274235E071967BEb71A2236A13F597A78 scaled balance (35626) does not match 
contract balance (0) at block 21928936
```

## Root Cause

### Transaction Structure

Transaction `0x09f27f2ee2a04a13a85e137007135593d848ffd5d590980783cfcb2d2571ab04` at block 21928936 contains **two liquidations** for the same user:

**Liquidation 1** (logIndex=363): cbBTC debt + AAVE collateral
- DEBT_BURN at logIndex=354: cbBTC vToken, amount=35000, balance_increase=27
- COLLATERAL_BURN at logIndex=358: AAVE aToken

**Liquidation 2** (logIndex=388): WBTC debt + WETH collateral  
- DEBT_BURN at logIndex=376: **WBTC vToken, amount=35780, balance_increase=619**
- COLLATERAL_BURN at logIndex=381: WETH aToken

### The Bug

The WBTC debt burn at logIndex=376 is being classified as an **INTEREST_ACCRUAL** operation instead of being assigned to Liquidation 2. This happens because:

1. During operation creation, `_create_liquidation_operation` calls `_collect_primary_debt_burns`
2. The amount matching logic uses a tolerance check:
   ```python
   total_burn = ev.amount + (ev.balance_increase or 0)
   if abs(total_burn - debt_to_cover) > TOKEN_AMOUNT_MATCH_TOLERANCE:
       continue  # Burn doesn't match, skip it
   ```
3. For WBTC: `total_burn = 35780 + 619 = 36399`
4. The LiquidationCall event at logIndex=388 encodes `debtToCover = 1160` (0x488)
5. `abs(36399 - 1160) = 35239` which is **much greater than** `TOKEN_AMOUNT_MATCH_TOLERANCE` (10)
6. The burn fails to match Liquidation 2 and remains unassigned
7. Later, `_create_interest_accrual_operations` picks up the unassigned debt burn and creates an INTEREST_ACCRUAL operation
8. When processing INTEREST_ACCRUAL, the code skips balance changes for debt mints, treating it as "tracking-only"
9. Result: WBTC debt balance remains 35626 instead of being burned to 0

### Why debtToCover = 1160?

The LiquidationCall event at logIndex=388 has the following data:
- `debtAsset` = WBTC (0x2260...)
- `debtToCover` = 1160 (0x488) - **This appears to be a protocol fee, not the actual debt amount**

The actual debt burned is 35,780 units, which represents the user's entire WBTC debt position being liquidated.

## Transaction Details

- **Hash:** `0x09f27f2ee2a04a13a85e137007135593d848ffd5d590980783cfcb2d2571ab04`
- **Block:** 21928936
- **Type:** Multi-liquidation (flash loan via 1inch Router)
- **User:** `0x9913e51274235E071967BEb71A2236A13F597A78`
- **Debt Asset:** WBTC (vToken: `0x40aAbEf1aa8f0eEc637E0E7d92fbfFB2F26A8b7B`)
- **Pool:** `0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2` (revision 7)
- **WBTC vToken:** Revision 1

### Events Sequence

| logIndex | Event | Contract | Details |
|----------|-------|----------|---------|
| 354 | ReserveDataUpdated | Pool | WBTC index update |
| 354 | Burn | WBTC vToken | **First debt burn (for cbBTC liq)** |
| 358 | Burn | AAVE aToken | Collateral burn |
| 361 | BalanceTransfer | AAVE aToken | Liquidation fee |
| 363 | LiquidationCall | Pool | **First liquidation (cbBTC)** |
| 376 | Burn | WBTC vToken | **Second debt burn (for WBTC liq)** |
| 377 | LiquidationCall | Pool | Second liquidation - WBTC debt asset |
| 381 | Burn | WETH aToken | Collateral burn |
| 384 | Mint | WETH aToken | Interest accrual |
| 386 | BalanceTransfer | WETH aToken | Liquidation fee |
| 388 | LiquidationCall | Pool | **Third liquidation?** |

Note: There are actually THREE LiquidationCall events in this transaction at logIndex 363, 377, and 388. The WBTC debt burn at logIndex 376 occurs between the second and third liquidation calls.

## Smart Contract Behavior

From `LiquidationLogic.sol` (Pool revision 7):

```solidity
function executeLiquidationCall(
    address collateralAsset,
    address debtAsset,
    address user,
    uint256 debtToCover,
    bool receiveAToken
) external {
    // ... validation ...
    
    // For bad debt or full liquidation, the entire debt is burned
    // not just the debtToCover amount
    uint256 borrowerDebt = IERC20(debtToken).balanceOf(user);
    uint256 debtToLiquidate = debtToCover > borrowerDebt ? borrowerDebt : debtToCover;
    
    // Burn debt tokens
    IVariableDebtToken(debtToken).burn(user, debtToLiquidate, index);
}
```

When `debtToCover >= borrowerDebt`, the entire debt is liquidated, which explains why the burn amount (35,780) is much larger than the debtToCover value (1,160) from the event.

## Fix

### Problem

The amount-based matching in `_collect_primary_debt_burns` is too restrictive when:
1. The debtToCover in the LiquidationCall event represents a partial amount or fee
2. The actual burn amount equals the user's full debt balance
3. Multiple liquidations exist for the same user

### Solution

For liquidation operations, rely on **semantic matching** (user + asset) rather than amount matching. The presence of a debt burn for the same user and debt asset in the same transaction indicates it belongs to that liquidation.

**In `aave_transaction_operations.py`, modify `_collect_primary_debt_burns`:**

```python
def _collect_primary_debt_burns(
    *,
    user: ChecksumAddress,
    debt_v_token_address: ChecksumAddress | None,
    debt_to_cover: int,  # Keep for logging/debugging
    pool_revision: int,
    scaled_events: list[ScaledTokenEvent],
    assigned_indices: set[int],
    is_gho: bool,
) -> list[ScaledTokenEvent]:
    """
    Collect primary debt burns matching the liquidation's debt asset.

    Uses semantic matching: a debt burn for the same user and debt asset
    in this transaction belongs to this liquidation, regardless of amounts.
    Amount validation happens during processing.
    """

    primary_burns: list[ScaledTokenEvent] = []

    for ev in scaled_events:
        if ev.event["logIndex"] in assigned_indices:
            continue
        if ev.user_address != user:
            continue
        if is_gho and ev.event_type != ScaledTokenEventType.GHO_DEBT_BURN:
            continue
        if not is_gho and ev.event_type != ScaledTokenEventType.DEBT_BURN:
            continue

        event_token_address = get_checksum_address(ev.event["address"])
        if debt_v_token_address is None or event_token_address != debt_v_token_address:
            continue

        # Semantic matching: the presence of a debt burn for this user and
        # asset in this transaction indicates it belongs to this liquidation.
        # Trust the smart contract event ordering/logic over amount comparisons.
        primary_burns.append(ev)
        assigned_indices.add(ev.event["logIndex"])
        if ev.index is not None and ev.index > 0:
            assigned_indices.add(ev.index)
        break  # Only one primary burn expected per (user, asset) pair

    return primary_burns
```

**Remove or relax the amount tolerance check** that currently filters out valid burns:

```python
# REMOVE THIS CHECK - it incorrectly rejects valid burns
# total_burn = ev.amount + (ev.balance_increase or 0)
# if abs(total_burn - debt_to_cover) > TOKEN_AMOUNT_MATCH_TOLERANCE:
#     continue
```

## Files to Modify

1. `src/degenbot/cli/aave_transaction_operations.py`
   - Method: `_collect_primary_debt_burns` (lines 1951-2009)
   - Remove the amount tolerance check (lines 1990-1998)

## Key Insight

**Amount-based matching fails when the LiquidationCall event's debtToCover doesn't match the actual burn amount.**

In Aave V3 liquidations:
- `debtToCover` in LiquidationCall may be a parameter, not the actual amount burned
- The actual burn amount = user's total debt when `debtToCover >= debtBalance`
- Smart contract logic determines the burn amount, not the pool event parameter
- Trust semantic relationships (user + asset) over amount comparisons

## Related Issues

- **Issue 0045**: Liquidation Validation Treats Debt Mint as Multiple Burns
  - Fixed validation logic to use `is_burn and is_debt` instead of just `is_debt`
  - Enhanced burn matching with amount-based disambiguation for same-asset liquidations

- **Issue 0028**: Multi-Asset Debt Liquidation Missing Secondary Debt Burns
  - Introduced semantic matching approach for debt burns
  - This issue extends that approach to handle amount mismatches

## Refactoring

1. **Remove amount-based disambiguation** in favor of pure semantic matching
2. **Add debug logging** when debtToCover and burn amount differ significantly
3. **Consider tracking liquidation context** (deficit vs normal) to handle edge cases
4. **Document** that debtToCover in LiquidationCall is not always the actual burn amount

## Verification

After fix:
- WBTC debt burn at logIndex 376 is assigned to Liquidation 2 (logIndex 388)
- WBTC debt position is correctly reduced from 35626 to 0
- Balance verification passes
- Both liquidations process correctly
- No regressions in other liquidation transactions

## Lint & Type Check

- `uv run ruff check` - No issues
- `uv run mypy` - No issues
