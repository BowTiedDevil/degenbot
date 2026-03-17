# MINT_TO_TREASURY Simplified Direct Query Approach

## Issue

The `_calculate_mint_to_treasury_scaled_amount` function was using complex reverse calculations to derive the scaled amount from event data. This was error-prone and difficult to maintain due to:

1. **Integer division irreversibility**: You cannot perfectly reverse a floor division operation
2. **Complex formulas**: Required maintaining different formulas for different AToken revisions
3. **Position data dependency**: Required access to user's previous balance and stored index

## Solution

Query `reserve.accruedToTreasury` directly from the Pool contract before it was reset to 0.

## Implementation

**File:** `src/degenbot/cli/aave.py`

### New Helper Function

```python
def _get_accrued_to_treasury_from_pool(
    w3: Web3,
    pool_address: ChecksumAddress,
    underlying_asset_address: ChecksumAddress,
    block_number: int,
) -> int | None:
    """Query accruedToTreasury directly from Pool contract."""
```

This function:
1. Calls `Pool.getReserveData(asset)` at `block_number - 1`
2. Decodes the returned struct to extract `accruedToTreasury` (index 12)
3. Returns the exact scaled amount that was minted to the treasury

### Updated Main Function

```python
def _calculate_mint_to_treasury_scaled_amount(
    scaled_event: ScaledTokenEvent,
    collateral_position: AaveV3CollateralPosition,
    balance_transfer_events: list[LogReceipt],
    tx_context: TransactionContext,
    collateral_asset: AaveV3Asset,
) -> int:
```

**New logic:**
1. Handle BalanceTransfer (if present) - use directly
2. Find Pool contract by name (not index - contracts aren't ordered)
3. Query `accruedToTreasury` from Pool contract - **exact scaled amount**
4. Fallback to calculation if RPC query fails

### Bug Fix

**Issue:** Original code used `tx_context.market.contracts[0]` assuming the Pool was first in the list.

**Fix:** Use `next((c for c in contracts if c.name == "POOL"), None)` to find the Pool contract by name.

**Why:** The `contracts` list is not ordered, so `contracts[0]` could be any contract (Pool Address Provider, Data Provider, etc.).

## Why This Works

**Contract behavior (All Pool Revisions):**
```solidity
function executeMintToTreasury(...) external {
    uint256 accruedToTreasury = reserve.accruedToTreasury;  // Read SCALED amount
    if (accruedToTreasury != 0) {
        reserve.accruedToTreasury = 0;  // Reset to 0
        ...
        IAToken(reserve.aTokenAddress).mintToTreasury(accruedToTreasury, ...);
        // AToken mints exactly accruedToTreasury scaled tokens
    }
}
```

**Key insight:** The `accruedToTreasury` value in storage IS the scaled amount that gets minted.

**Struct Layout (All Revisions):**

The `getReserveData()` function returns the same struct layout for all Pool revisions:
- Rev 1-3: Returns `ReserveData`
- Rev 4+: Returns `ReserveDataLegacy` (maintains backward compatibility)

In both cases, `accruedToTreasury` is at **index 12**:
```
Index 12: accruedToTreasury (uint128) <- The SCALED amount
```

This is possible because `ReserveDataLegacy` was specifically designed to maintain the same interface as the original `ReserveData` struct.

**Tenderly verification (Transaction 0xe921b7eea5cb014e6253835f0929c41123e424947c979b70e32aae164a4551e2):**

| Value | Amount |
|-------|--------|
| `reserve.accruedToTreasury` | 71,484,367,636,514,015,634 |
| Passed to `AToken.mintToTreasury()` | 71,484,367,636,514,015,634 |
| MintedToTreasury event | 75,108,614,259,086,883,884 (UNSCALED) |
| AToken Mint event | 75,117,324,494,890,597,110 (UNSCALED + interest) |

**Conclusion:** The scaled amount (71.48 ETH worth) was never emitted in any event, but it was stored in the Pool contract and reset to 0 after the transaction.

## Benefits

1. **Simplicity**: No complex reverse calculations
2. **Accuracy**: Exact value from contract storage
3. **Maintainability**: Single approach for all revisions
4. **Performance**: mintToTreasury is rare, RPC cost is negligible

## Fallback Strategy

If the RPC query fails (e.g., contract not available), the function falls back to the calculation method:
- AToken Rev 1-3: Simple formula with half-up rounding
- AToken Rev 4+: Complex formula with floor/ceil rounding

This ensures the system continues to work even if RPC queries fail.

## Files Changed

1. `src/degenbot/cli/aave.py`
   - Added `_get_accrued_to_treasury_from_pool()` helper
   - Simplified `_calculate_mint_to_treasury_scaled_amount()`
   - Removed incorrect Rev 9+ optimization
   - Works uniformly across ALL Pool revisions (same struct layout)

## Testing

All existing tests pass. The new approach was verified against Tenderly trace data.

## References

- Issue 0014: Original complex formula implementation
- Issue 0020: Version check refactoring
- Contract: `contract_reference/aave/Pool/rev_9.sol:109-134`
- Transaction: `0xe921b7eea5cb014e6253835f0929c41123e424947c979b70e32aae164a4551e2`
