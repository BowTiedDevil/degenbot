# 0043 - Multi-Liquidation Secondary Debt Burn Misclassification

## Issue
Multi-liquidation transaction fails balance verification when a user is liquidated multiple times with different debt assets.

## Date
2026-03-20

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...). User AaveV3User(...) scaled balance (943382304624) does not match contract balance (667029376406) at block 20459073
```

**Key Data Points:**
- Calculated balance: 943,382,304,624
- On-chain balance: 667,029,376,406
- Difference: 276,352,928,218 (significant mismatch)

## Root Cause

When a user is liquidated multiple times in a single transaction with **different debt assets**, the debt burns for the second liquidation are incorrectly classified as "secondary" burns for the first liquidation.

### Transaction Structure

**Transaction:** `0xc50f86c14e5b72ddc011b101a41353ac8f69ff6605b00960f1637413df687f9f`
**Block:** 20459073
**User:** `0x0B5a6a15B975fD35f0B301748C8DaBD35b50d8C5`

The transaction contains 4 liquidation operations:
1. **Operation 0:** LIQUIDATION - USDT debt + WETH collateral (user: 0x0B5a6a15...)
2. **Operation 1:** LIQUIDATION - USDT debt + WBTC collateral (user: 0x6Dc12064...)
3. **Operation 2:** LIQUIDATION - USDC debt + WETH collateral (user: 0x0B5a6a15...)
4. **Operation 3:** LIQUIDATION - WETH debt + WETH collateral (user: 0x502919D6...)

### The Bug

**Step 1:** When creating Operation 0 (USDT liquidation), `_create_liquidation_operation` calls:
- `_collect_primary_debt_burns`: Finds and assigns the USDT debt burn (correct)
- `_collect_secondary_debt_burns`: Finds ALL other debt burns for the user, including the USDC debt burn (incorrect)

**Step 2:** The USDC debt burn is now assigned to Operation 0 as a "secondary" burn.

**Step 3:** When creating Operation 2 (USDC liquidation), `_collect_primary_debt_burns` finds NO unassigned USDC debt burn (it was already assigned as "secondary" to Operation 0).

**Step 4:** During enrichment, the USDC debt burn in Operation 0 uses the USDT liquidation's `debtToCover` (116,511,830,272) instead of the USDC liquidation's `debtToCover` (421,941,327,248).

**Step 5:** The debt processor calculates an incorrect burn amount, leading to the balance mismatch.

### Code Analysis

**In `aave_transaction_operations.py:1996-2038`:**

```python
def _collect_secondary_debt_burns(
    self,
    *,
    user: ChecksumAddress,
    debt_v_token_address: ChecksumAddress | None,
    scaled_events: list[ScaledTokenEvent],
    assigned_indices: set[int],
    is_gho: bool,
) -> list[ScaledTokenEvent]:
    """
    Collect secondary debt burns for other assets held by the user.
    ...
    """
    secondary_burns: list[ScaledTokenEvent] = []

    for ev in scaled_events:
        if ev.event["logIndex"] in assigned_indices:
            continue
        if ev.user_address != user:
            continue
        # Collect ALL debt burn types (both GHO and non-GHO) as secondary burns
        if ev.event_type not in {
            ScaledTokenEventType.DEBT_BURN,
            ScaledTokenEventType.GHO_DEBT_BURN,
        }:
            continue

        event_token_address = get_checksum_address(ev.event["address"])
        if debt_v_token_address is not None and event_token_address == debt_v_token_address:
            continue  # Skip primary debt burns

        # Validate this is a real debt token
        asset = self._get_asset_by_v_token(event_token_address)
        if asset is not None:
            secondary_burns.append(ev)  # BUG: This collects ALL other debt burns

    return secondary_burns
```

The problem is that `_collect_secondary_debt_burns` collects ALL debt burns for the user that aren't the primary debt asset, without checking if those burns belong to another liquidation in the same transaction.

### Secondary Debt Burn Semantics

Secondary debt burns should only be collected when they represent **bad debt write-offs** (same liquidation, additional debts being cleared). They should NOT be collected when they belong to a **separate liquidation** of a different debt asset.

**Correct secondary burn scenario:**
- User has USDT and USDC debt
- Single liquidation call liquidates USDT debt
- Both USDT and USDC burns occur (bad debt write-off)
- USDC burn is secondary to the USDT liquidation

**Incorrect secondary burn scenario (current bug):**
- User has USDT and USDC debt
- Two separate liquidation calls: one for USDT, one for USDC
- USDC burn is incorrectly collected as secondary to USDT liquidation
- USDC liquidation has no debt burn

## Transaction Details

- **Hash:** `0xc50f86c14e5b72ddc011b101a41353ac8f69ff6605b00960f1637413df687f9f`
- **Block:** 20459073
- **Type:** Multi-Liquidation (4 simultaneous liquidations)
- **Pool Revision:** 4
- **Token Revisions:** aToken=1, vToken=1

### Debt Burns for User 0x0B5a6a15...

| Log Index | Asset | Amount | Current Assignment | Should Be |
|-----------|-------|--------|-------------------|-----------|
| 2 | USDT | 116,510,549,636 | Op 0 (primary) | Op 0 (primary) |
| 38 | USDC | 421,941,327,248 | Op 0 (secondary) | Op 2 (primary) |

## Fix

**File:** `src/degenbot/cli/aave_transaction_operations.py`

**Location:** Lines 1996-2038 (`_collect_secondary_debt_burns` method)

**Change:** Skip collecting secondary debt burns when multiple liquidations exist for the same user in the transaction. This prevents misclassification when a user is liquidated multiple times with different debt assets.

```python
def _collect_secondary_debt_burns(
    self,
    *,
    user: ChecksumAddress,
    debt_v_token_address: ChecksumAddress | None,
    scaled_events: list[ScaledTokenEvent],
    assigned_indices: set[int],
    is_gho: bool,
    user_liquidation_count: int = 1,  # NEW: Pass liquidation count
) -> list[ScaledTokenEvent]:
    """
    Collect secondary debt burns for other assets held by the user.
    
    Secondary burns represent bad debt write-offs where a single liquidation
    clears multiple debt positions. When multiple liquidations exist for the
    same user, each liquidation handles only its primary debt - secondary
    burns are skipped to avoid misclassification.
    """
    # Skip secondary burns when multiple liquidations exist
    # Each liquidation handles its own primary debt
    if user_liquidation_count > 1:
        return []
    
    # ... rest of existing logic unchanged
```

**Update the call site in `_create_liquidation_operation` (lines ~2131-2146):**

```python
# Count liquidations for this user
user_liquidation_count = sum(
    1 for ev in all_events
    if ev["topics"][0] == AaveV3PoolEvent.LIQUIDATION_CALL.value
    and decode_address(ev["topics"][3]) == user
)

primary_burns = self._collect_primary_debt_burns(...)
secondary_burns = self._collect_secondary_debt_burns(
    user=user,
    debt_v_token_address=debt_v_token_address,
    scaled_events=scaled_events,
    assigned_indices=assigned_indices,
    is_gho=is_gho,
    user_liquidation_count=user_liquidation_count,  # Pass count
)
```

## Key Insight

**Secondary debt burns and multi-liquidations are mutually exclusive scenarios:**

- **Secondary burns** occur when a single liquidation clears multiple debt positions (bad debt write-off)
- **Multi-liquidations** occur when separate liquidation calls target different debt positions

These scenarios cannot happen simultaneously for the same user in the same transaction. Therefore, when multiple liquidations exist for a user, each should only process its primary debt burn.

## Refactoring Recommendations

1. **Add transaction-level liquidation analysis:**
   - Count liquidations per user at the start of transaction processing
   - Adjust debt burn collection strategy based on liquidation count

2. **Improve operation creation logging:**
   - Log when secondary burns are skipped due to multi-liquidation scenario
   - Log debt burn assignments to help debug matching issues

3. **Add validation:**
   - Verify that each liquidation has exactly one primary debt burn
   - Verify that secondary burns don't match other liquidations' debt assets

4. **Consider edge cases:**
   - What if a user has 3+ debt positions and 2 are liquidated separately?
   - What if liquidations are interleaved with other operations?

## Verification

**Test Result:** ✅ PASSED

```bash
$ uv run degenbot aave update
...
AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True) successfully updated to block 20,459,073
```

**After the fix:**
- USDT debt burn correctly assigned to Operation 0 as primary
- USDC debt burn correctly assigned to Operation 2 as primary  
- User's USDC debt balance correctly reduced by 421,941,327,248
- Final balance matches on-chain balance: 667,029,376,406
- Verification passes at block 20459073

**Code changes:**
- Modified `_collect_secondary_debt_burns` to accept `user_liquidation_count` parameter
- Added early return when `user_liquidation_count > 1`
- Updated `_create_liquidation_operation` to count liquidations and pass the count
- Added `all_events` parameter to `_create_liquidation_operation` signature

**Files modified:**
- `src/degenbot/cli/aave_transaction_operations.py` (lines 1094-1100, 2089-2097, 1996-2046, ~2140-2166)

## Related Issues

- Issue 0028: Multi-Asset Debt Liquidation Missing Secondary Debt Burns
- Issue 0029: Multi-Asset Liquidation Missing Secondary Debt Burns Fix
- Issue 0042: Pool Revision 9 Liquidation Debt Amount Already Scaled

## Contract References

**Pool Revision 4 (rev_4.sol):**
- `liquidationCall()` can be called multiple times in a single transaction
- Each call emits a separate LIQUIDATION_CALL event
- Debt burns are per-liquidation-call, not aggregated

**VariableDebtToken Revision 1 (rev_1.sol):**
- `burn()` called once per liquidation
- Emits Burn event with scaled amount
