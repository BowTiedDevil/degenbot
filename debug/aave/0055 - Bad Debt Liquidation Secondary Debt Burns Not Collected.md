# Issue 0055: Bad Debt Liquidation Secondary Debt Burns Not Collected

## Date
2026-03-23

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), underlying_token=Erc20TokenTable(chain=1, address='0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2', symbol='WETH'), a_token=Erc20TokenTable(chain=1, address='0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8', symbol='aEthWETH'), v_token=Erc20TokenTable(chain=1, address='0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE', symbol='variableDebtEthWETH')). 
User AaveV3User(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), address='0x60a4DaDea54Fd242D11462667598A73473543542', e_mode=0) scaled balance (3587151217) does not match contract balance (0) at block 23042379
```

## Root Cause

In bad debt liquidations where the user has multiple debt positions, the `_collect_debt_burns` function only collects burns for the debt asset specified in the `LiquidationCall` event. It does not collect burns for other debt assets that are also burned via `_burnBadDebt()`.

### Contract Behavior

In Aave V3 bad debt liquidations, the protocol burns ALL debt positions, not just the specified debt asset:

```solidity
// Pool rev_8.sol lines 377-388
if (hasNoCollateralLeft && borrowerConfig.isBorrowingAny()) {
    _burnBadDebt(
        reservesData,
        reservesList,
        borrowerConfig,
        params.borrower,
        params.interestRateStrategyAddress
    );
}
```

The `_burnBadDebt()` function iterates through ALL reserves where the user has debt and burns each one.

### Event Sequence

**Transaction:** `0x1ebd5932aaf30f49ead905f204e60cb4e483492efd333b24a84d5d40f07b3d54`
**Block:** 23042379
**Type:** Bad Debt Liquidation with Multi-Asset Debt

| Log Index | Event | Contract | Details |
|-----------|-------|----------|---------|
| 106 | Burn | vEthUSDC | USDC debt burned (primary) |
| 107 | DeficitCreated | Pool | USDC deficit |
| 111 | Burn | aEthWstETH | Collateral burn |
| 116 | BalanceTransfer | aEthWstETH | Collateral to Treasury |
| 118 | Burn | vEthWETH | **WETH debt burned (secondary)** |
| 119 | DeficitCreated | Pool | **WETH deficit** |
| 122 | LiquidationCall | Pool | debtAsset=USDC, collateralAsset=wstETH |

### The Bug

In `_collect_debt_burns` (aave_transaction_operations.py:2019-2030):

```python
candidate_burns = sorted(
    [
        ev
        for ev in scaled_events
        if ev.event["logIndex"] not in assigned_indices
        and ev.user_address == user
        and ev.event_type
        in {ScaledTokenEventType.DEBT_BURN, ScaledTokenEventType.GHO_DEBT_BURN}
        and get_checksum_address(ev.event["address"]) == debt_v_token_address  # <-- PROBLEM
    ],
    key=lambda e: e.event["logIndex"],
)
```

The filter `event["address"] == debt_v_token_address` only matches burns for the debt asset in the LiquidationCall (USDC). The WETH debt burn at logIndex 118 has a different address (vEthWETH != vEthUSDC), so it is:
1. Not collected by `_collect_debt_burns`
2. Left unassigned
3. Picked up by `_create_interest_accrual_operations`
4. Processed as INTEREST_ACCRUAL instead of LIQUIDATION
5. The WETH debt position is NOT cleared
6. Balance verification fails

### What Happened

1. `_create_liquidation_operation` calls `_collect_debt_burns(debt_v_token_address=vEthUSDC)`
2. Only the USDC burn at logIndex 106 is collected (address matches)
3. The WETH burn at logIndex 118 is skipped (vEthWETH != vEthUSDC)
4. The unassigned WETH burn becomes Operation 3 (INTEREST_ACCRUAL)
5. When INTEREST_ACCRUAL is processed, the debt balance is reduced by the burn amount but NOT cleared to 0
6. The WETH debt position still has balance 3,587,151,217
7. On-chain balance is 0 (full debt was burned)
8. Verification fails

## Transaction Details

| Field | Value |
|-------|-------|
| **Hash** | 0x1ebd5932aaf30f49ead905f204e60cb4e483492efd333b24a84d5d40f07b3d54 |
| **Block** | 23042379 |
| **Type** | Bad Debt Liquidation (Multi-Asset Debt) |
| **User** | 0x60a4DaDea54Fd242D11462667598A73473543542 |
| **Primary Debt** | USDC (3,053,682 burned) |
| **Secondary Debt** | WETH (3,799,768,021 burned) |
| **Collateral** | wstETH (214,478,075 burned + 1,220,235 to Treasury) |
| **Pool Revision** | 8 |
| **vToken Revision** | 3 |

## Fix

**Status:** ✅ IMPLEMENTED AND VERIFIED

**File:** `src/degenbot/cli/aave_transaction_operations.py`

**Location:** `_collect_debt_burns` function, lines 1993-2043

**Problem:** The function only collects burns for `debt_v_token_address`, missing secondary debt burns when a single liquidation burns multiple debt positions via `_burnBadDebt()`.

**Solution:** Use user-level liquidation count to determine collection strategy:
- **Single liquidation per user** → Collect ALL debt burns (no asset filter)
- **Multiple liquidations per user** → Use asset filter + sequential matching

### Implementation

Add a helper function to count liquidations per user and modify `_collect_debt_burns`:

```python
@staticmethod
def _analyze_user_liquidation_count(all_events: list[LogReceipt]) -> dict[ChecksumAddress, int]:
    """
    Count total liquidations per user (not per user+asset pair).
    
    When a user has exactly 1 liquidation, ALL debt burns for that user belong
    to that single liquidation. This handles bad debt liquidations where the
    protocol burns multiple debt positions via _burnBadDebt().
    
    When a user has multiple liquidations, use asset-specific matching to
    disambiguate which burns belong to which liquidation.
    """
    counts: dict[ChecksumAddress, int] = {}
    for ev in all_events:
        if ev["topics"][0] == AaveV3PoolEvent.LIQUIDATION_CALL.value:
            user = decode_address(ev["topics"][3])
            checksum_user = get_checksum_address(user)
            counts[checksum_user] = counts.get(checksum_user, 0) + 1
    return counts


@staticmethod
def _collect_debt_burns(
    *,
    user: ChecksumAddress,
    debt_v_token_address: ChecksumAddress | None,
    scaled_events: list[ScaledTokenEvent],
    assigned_indices: set[int],
    liquidation_analysis: dict[tuple[ChecksumAddress, ChecksumAddress], int],
    user_liquidation_count: int = 1,
    liquidation_position: int = 0,
) -> list[ScaledTokenEvent]:
    """
    Collect debt burns for the liquidated user.

    Collection strategy:
    - Single liquidation per user: Collect ALL debt burns (no asset filter)
      This handles bad debt liquidations where _burnBadDebt() burns all debt positions.
    - Multiple liquidations per user: Use asset filter + sequential matching
      to disambiguate which burns belong to which liquidation.

    See debug/aave/0054 for sequential matching approach.
    See debug/aave/0051 for original refactoring.
    See debug/aave/0052 for removal of is_gho-based filtering.
    See debug/aave/0055 for user-level liquidation count approach.
    """
    burns: list[ScaledTokenEvent] = []

    if user_liquidation_count == 1:
        # Single liquidation: collect ALL debt burns for user
        # The protocol burns all debt positions in a single liquidation call
        candidate_burns = sorted(
            [
                ev
                for ev in scaled_events
                if ev.event["logIndex"] not in assigned_indices
                and ev.user_address == user
                and ev.event_type
                in {ScaledTokenEventType.DEBT_BURN, ScaledTokenEventType.GHO_DEBT_BURN}
            ],
            key=lambda e: e.event["logIndex"],
        )
        for ev in candidate_burns:
            burns.append(ev)
            assigned_indices.add(ev.event["logIndex"])
            if ev.index is not None and ev.index > 0:
                assigned_indices.add(ev.index)
    else:
        # Multiple liquidations: use asset filter + sequential matching
        is_multi_liquidation = False
        if debt_v_token_address is not None:
            is_multi_liquidation = liquidation_analysis.get((user, debt_v_token_address), 0) > 1

        candidate_burns = sorted(
            [
                ev
                for ev in scaled_events
                if ev.event["logIndex"] not in assigned_indices
                and ev.user_address == user
                and ev.event_type
                in {ScaledTokenEventType.DEBT_BURN, ScaledTokenEventType.GHO_DEBT_BURN}
                and get_checksum_address(ev.event["address"]) == debt_v_token_address
            ],
            key=lambda e: e.event["logIndex"],
        )

        if is_multi_liquidation and len(candidate_burns) > 1:
            if liquidation_position < len(candidate_burns):
                burns.append(candidate_burns[liquidation_position])
                assigned_indices.add(candidate_burns[liquidation_position].event["logIndex"])
        else:
            for ev in candidate_burns:
                burns.append(ev)
                assigned_indices.add(ev.event["logIndex"])
                if ev.index is not None and ev.index > 0:
                    assigned_indices.add(ev.index)

    return burns
```

### Key Changes

1. **Add `_analyze_user_liquidation_count()` helper** - Counts liquidations per user (not per asset pair)
2. **Add `user_liquidation_count` parameter** - Passed from `_create_liquidation_operation`
3. **Branch on user_liquidation_count**:
   - `== 1`: Collect ALL debt burns for user (no asset filter)
   - `> 1`: Use existing asset-filtered sequential matching

### Why This Approach Is Architecturally Clean

| Aspect | DeficitCreated check | User liquidation count |
|--------|---------------------|------------------------|
| **Directness** | Indirect (side effect) | Direct (operation structure) |
| **Relies on** | Emission of deficit event | How many liquidations exist |
| **Applies to** | Bad debt only | Any single-liquidation scenario |
| **Principle** | Infer from byproduct | Model operation semantics |

The user count approach directly models: **one liquidation = all burns belong to it**.

## Key Insight

**Liquidation operation semantics: one liquidation = all burns belong to it.**

When a user has a single liquidation call, ALL debt burns for that user belong to that liquidation. This is true because:

1. **Normal liquidation:** Burns only the specified debt asset
2. **Bad debt liquidation:** Calls `_burnBadDebt()` which burns ALL debt assets

The code can't know which case it is at collection time, but it doesn't need to. If there's only one liquidation for the user, all burns belong to it regardless of how many assets are involved.

**The user-level liquidation count is the key signal:**
- `user_liquidation_count == 1` → Collect all debt burns for user (no asset filter)
- `user_liquidation_count > 1` → Use asset filter + sequential matching

This is more principled than checking for `DeficitCreated` events because it directly models operation semantics rather than inferring from side effects.

## Refactoring

The fix introduces a cleaner separation of concerns:

| Analysis Level | Purpose | Used For |
|---------------|---------|----------|
| `user_liquidation_count` | How many liquidations for this user? | Determines collection strategy |
| `liquidation_analysis` | How many liquidations for (user, asset)? | Sequential matching within asset |

**Collection strategy by scenario:**

| Scenario | user_liquidation_count | Strategy |
|----------|----------------------|----------|
| Single liquidation, single debt | 1 | Collect all burns (no filter) |
| Single liquidation, multi debt (bad debt) | 1 | Collect all burns (no filter) |
| Multi liquidation, same debt asset | >1 | Asset filter + sequential |
| Multi liquidation, different debt assets | >1 | Asset filter (each gets own burns) |

## Verification

**Test command:**
```bash
uv run degenbot aave update
```

**Result:** ✅ PASSED

**Before fix:** WETH debt burn at logIndex 118 was classified as INTEREST_ACCRUAL, leaving balance at 3,587,151,217.

**After fix:**
```
Operation 2: LIQUIDATION
  Scaled event: logIndex=111, type=ScaledTokenEventType.COLLATERAL_BURN
  Scaled event: logIndex=106, type=ScaledTokenEventType.DEBT_BURN      ← USDC
  Scaled event: logIndex=118, type=ScaledTokenEventType.DEBT_BURN      ← WETH (now collected!)
  ...
[Pool rev 8] Processing USDC debt burn at block 23042379
_process_debt_burn_with_match: BAD DEBT LIQUIDATION - setting balance to 0 (was 2579106)
[Pool rev 8] Processing WETH debt burn at block 23042379
_process_debt_burn_with_match: BAD DEBT LIQUIDATION - setting balance to 0 (was 3587151217)
AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True) successfully updated to block 23,042,379
```

Both debt positions (USDC and WETH) are now properly cleared during the bad debt liquidation.

## Related Issues

- Issue 0054: Multi-Liquidation Similar Amounts Range Overlap (sequential matching for same asset)
- Issue 0052: GHO Liquidation Skips Secondary Debt Burns (removed is_gho filter, but didn't add secondary collection)
- Issue 0051: Bad Debt Liquidation Debt Burn Matching Uses Only Principal Amount
- Issue 0028/0029: Multi-Asset Liquidation Missing Secondary Debt Burns (original secondary burn logic)

## References

- Contract: `contract_reference/aave/Pool/rev_8.sol` (lines 377-388, 638-676)
- File: `src/degenbot/cli/aave_transaction_operations.py`
  - `_analyze_user_liquidation_count()` (lines 1993-2015)
  - `_collect_debt_burns()` (lines 2017-2095)
  - `_create_liquidation_operation()` (lines 2178-2218)
- Transaction: 0x1ebd5932aaf30f49ead905f204e60cb4e483492efd333b24a84d5d40f07b3d54
- Investigation report: `/tmp/aave_tx_report.md`
