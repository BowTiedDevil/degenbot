# Issue: Bad Debt Liquidation Burns Full Debt Balance

## Date
2026-03-16

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...). 
User AaveV3User(...) scaled balance (21869388958269) 
does not match contract balance (0) at block 23006794
```

## Root Cause

In **bad debt liquidations** (when a DEFICIT_CREATED event is emitted), the Aave protocol burns the **entire debt balance** (borrowerReserveDebt), not just the `debtToCover` amount specified in the LiquidationCall event.

### How Bad Debt Liquidations Work

From the Aave Pool contract (`LiquidationLogic.sol`):

```solidity
// Burn the debt
burn(
    borrower, 
    hasNoCollateralLeft ? borrowerReserveDebt : actualDebtToLiquidate, 
    index
);

// Calculate and record deficit (AFTER burn)
uint256 outstandingDebt = borrowerReserveDebt - actualDebtToLiquidate;
if (hasNoCollateralLeft && outstandingDebt != 0) {
    debtReserve.deficit += outstandingDebt.toUint128();
    emit DeficitCreated(borrower, debtAsset, outstandingDebt);
}
```

When `hasNoCollateralLeft=true`:
- The liquidator pays `actualDebtToLiquidate` (debtToCover from LiquidationCall event)
- The protocol burns `borrowerReserveDebt` (the entire debt, including interest)
- The difference is recorded as a deficit (bad debt)

### The Bug

The code was using `debtToCover` from the LiquidationCall event as the burn amount, but in bad debt liquidations, the Burn event shows the full debt amount being burned. This caused the calculated balance to be off by ~120 billion wei.

## Transaction Details

| Field | Value |
|-------|-------|
| **Transaction Hash** | 0x60fc29caf1fba97df8802d6407b8c91a11ec8fdd44d77c87a07296c4f087fe8e |
| **Block** | 23006794 |
| **Type** | Bad Debt Liquidation |
| **User** | 0x6b7Ccc4528e27e33094b7b906B2fC36fA246aE8f |
| **Debt Asset** | WETH (0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2) |
| **vToken** | 0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE |
| **Pool Revision** | 8 |
| **vToken Revision** | 3 |
| **LiquidationCall debtToCover** | 1,753,077,485,613,466 wei (0.001753 ETH) |
| **Burn event amount** | 1,769,734,354,180,657 wei (0.001777 ETH) |
| **Balance increase** | 6,896,983,603,599 wei |
| **Starting balance** | 1,649,574,833,013,428 scaled units |
| **Expected after burn** | 0 scaled units |

### Event Sequence

1. **LiquidationCall** (logIndex 753): debtToCover=1,753,077,485,613,466
2. **DEFICIT_CREATED** (logIndex 754): Indicates bad debt liquidation
3. **Debt Burn** (logIndex 752): Burns full debt 1,769,734,354,180,657
4. **Collateral Burn** (logIndex 757): Burns collateral
5. **BalanceTransfer** (logIndex 762): Liquidation fee to treasury

## Fix

**File**: `src/degenbot/cli/aave.py`

**Location**: `_process_debt_burn_with_match` function

**Change**: Detect bad debt liquidations by checking for DEFICIT_CREATED event and handle them differently:

```python
# Check if this is a bad debt liquidation (has DEFICIT_CREATED event)
is_bad_debt_liquidation = False
if operation and operation.operation_type in {
    OperationType.LIQUIDATION,
    OperationType.GHO_LIQUIDATION,
}:
    if tx_context is not None:
        for evt in tx_context.events:
            if evt["topics"][0] == AaveV3PoolEvent.DEFICIT_CREATED.value:
                deficit_user = get_checksum_address("0x" + evt["topics"][1].hex()[-40:])
                if deficit_user == user.address:
                    is_bad_debt_liquidation = True
                    break

if is_bad_debt_liquidation:
    # Bad debt liquidation: Set debt balance to 0 (full debt is burned)
    debt_position.balance = 0
    debt_position.last_index = scaled_event.index
    return
elif operation and operation.operation_type in {
    OperationType.LIQUIDATION,
    OperationType.GHO_LIQUIDATION,
}:
    # Normal liquidation: use debtToCover from pool event
    burn_value = enriched_event.raw_amount
else:
    # Standard REPAY: use Burn event value
    burn_value = scaled_event.amount
```

## Key Insight

**Bad debt liquidations vs Normal liquidations:**

- **Normal liquidations**: The debt is reduced by `debtToCover` amount. User may still have remaining debt.
- **Bad debt liquidations**: The entire debt is written off (set to 0). The protocol absorbs the loss.

The presence of a `DEFICIT_CREATED` event is the signal that this is a bad debt liquidation.

## Verification

**Test Results**:
- Block 23006794: ✅ Passed (original failing block)
- Blocks 23006794-23006894: ✅ 100 blocks passed

## Related Issues

- Issue 0014: MINT_TO_TREASURY AccruedToTreasury Calculation Error
- Issue 0015: MINT_TO_TREASURY BalanceTransfer Amount Not Used
- Issue 0016: REPAY with Interest Exceeding Repayment Uses Wrong Rounding
- Issue 0017: MINT_TO_TREASURY Validation Error on Pool Revision 8

## References

- Contract: `contract_reference/aave/Pool/rev_8.sol` (LiquidationLogic)
- Contract: `contract_reference/aave/VariableDebtToken/rev_3.sol` (burn function)
- Event: `DEFICIT_CREATED` (0x2bccfb3fad376d59d7accf970515eb77b2f27b082c90ed0fb15583dd5a942699)
- Files:
  - `src/degenbot/cli/aave.py` (debt burn processing)
  - `src/degenbot/cli/aave_transaction_operations.py` (operation creation)
