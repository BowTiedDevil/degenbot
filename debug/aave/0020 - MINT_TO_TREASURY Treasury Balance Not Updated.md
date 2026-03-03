# Issue: MINT_TO_TREASURY Treasury Balance Not Updated

**Issue ID:** 0020
**Date:** 2026-03-03
**Status:** Fixed

---

## Symptom

```
AssertionError: User 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c: collateral balance (85612676842700342505) does not match scaled token contract (85954719454521124881) @ 0x0B925eD163218f6662a35e0f0371Ac234f9E9371 at block 20282197
```

---

## Root Cause

The MINT_TO_TREASURY operation processing had two complementary bugs:

1. **Zero scaled_amount**: The code set `scaled_amount = 0` for MINT_TO_TREASURY operations (lines 2681-2687 in `aave.py`), assuming BalanceTransfer events would handle the balance updates.

2. **Transfer events skipped**: ERC20 Transfer events from `address(0)` (representing mints) were explicitly skipped at line 3196 with the comment "These are protocol reserve mints via mintToTreasury()".

The combined effect was that NO balance update occurred for treasury mints:
- Mint events had `scaled_amount = 0` → no balance delta
- Transfer events were skipped → no balance update
- Treasury collateral positions were never credited with minted aTokens

The Mint event contains the actual minted amount (post-interest), while the MintedToTreasury Pool event contains only the pre-interest amount. The correct approach is to calculate the scaled balance from the Mint event data: `(amount - balance_increase) / index`.

---

## Transaction Details

| Field | Value |
|-------|-------|
| **Transaction Hash** | `0xd51e5b48833371521c039bbfedb8d120588bb41169684cac7df09fd32cc8ad7f` |
| **Block Number** | 20282197 |
| **Timestamp** | 2024-07-11 08:54:11 UTC |
| **Transaction Type** | Multi-asset treasury mint (24 reserves) |
| **User** | 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c (Aave Treasury) |
| **Token** | 0x0B925eD163218f6662a35e0f0371Ac234f9E9371 (awstETH) |
| **Mint Event Amount** | 342,377,914,964,639,358 (0.342 awstETH) |
| **Balance Increase** | 146,202,171,318,490 (interest portion) |
| **Principal Amount** | 342,231,712,793,320,868 (pre-interest) |
| **Contract Balance** | 85,954,719,454,521,124,881 |
| **Database Balance** | 85,612,676,842,700,342,505 |
| **Difference** | 342,042,611,820,782,376 |

---

## Fix

**File:** `src/degenbot/cli/aave.py`  
**Lines:** 2681-2687

**Before:**
```python
elif operation.operation_type == OperationType.MINT_TO_TREASURY:
    # For MINT_TO_TREASURY, the Mint event represents accrued interest
    # However, investigation shows the BalanceTransfer events already include
    # the correct scaled amounts. The Mint event amount is interest calculation
    # that doesn't result in actual token transfers beyond what's in BalanceTransfer.
    # Skip adding anything here - the transfers will handle the balance changes.
    scaled_amount = 0
```

**After:**
```python
elif operation.operation_type == OperationType.MINT_TO_TREASURY:
    # For MINT_TO_TREASURY, the Mint event amount is the actual minted amount
    # (post-interest), while the MintedToTreasury Pool event shows pre-interest.
    # The Transfer event from address(0) is skipped, so we must calculate
    # the scaled amount from the Mint event data here.
    # Formula: scaled_amount = (value - balance_increase) / index
    # This gives the principal amount converted to scaled balance.
    collateral_processor = TokenProcessorFactory.get_collateral_processor(
        collateral_asset.a_token_revision
    )
    wad_ray_math = collateral_processor.get_math_libraries()["wad_ray"]
    principal_amount = scaled_event.amount - scaled_event.balance_increase
    scaled_amount = wad_ray_math.ray_div(principal_amount, scaled_event.index)
```

---

## Key Insight

When diagnosing "database balance != contract balance" errors, always trace through ALL code paths that could update the balance:

1. **Primary event path**: Mint/Burn events processed through `_process_scaled_token_operation`
2. **Secondary event path**: Transfer events processed through `_process_collateral_transfer_with_match`
3. **Edge case handling**: Check for `continue`, `return`, or `pass` statements that skip processing

In this case, both paths were disabled for MINT_TO_TREASURY operations:
- Mint events had `scaled_amount = 0` (no-op)
- Transfer events from `address(0)` were skipped

The "FUNDAMENTAL PREMISE" holds: the database was missing updates, not containing invalid data. The contract values are always authoritative.

---

## Refactoring

Consider refactoring MINT_TO_TREASURY handling to make the intent clearer:

1. **Rename variables**: Instead of `scaled_amount = 0`, use explicit flags like `skip_balance_update = True` to make the intent clear
2. **Consolidate logic**: The decision to skip Transfer events from `address(0)` is tightly coupled to the MINT_TO_TREASURY logic - document this relationship
3. **Validation**: Add assertions to ensure at least one code path updates the balance for each operation type
4. **Event pairing**: Explicitly pair Mint events with their corresponding BalanceTransfer events during operation creation, rather than relying on index proximity matching

---

## Verification

- Fix validated at block 20282197: ✓
- All existing tests pass: ✓ (5/5 MINT_TO_TREASURY tests, 21/21 related tests)
- No regressions detected: ✓

---

## References

- Transaction: https://etherscan.io/tx/0xd51e5b48833371521c039bbfedb8d120588bb41169684cac7df09fd32cc8ad7f
- Token: https://etherscan.io/address/0x0B925eD163218f6662a35e0f0371Ac234f9E9371
- Investigation report: `/tmp/0020_transaction_investigation.md`
