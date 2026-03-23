# Issue 0052: GHO Liquidation Skips Secondary Debt Burns

## Date
2026-03-23

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), underlying_token=Erc20TokenTable(chain=1, address='0x7f39C581F595B53c5cb19bD0b3f8dA6c935E2Ca0', symbol='wstETH'), a_token=Erc20TokenTable(chain=1, address='0x0B925eD163218f6662a35e0f0371Ac234f9E9371', symbol='aEthwstETH'), v_token=Erc20TokenTable(chain=1, address='0xC96113eED8cAB59cD8A66813bCB0cEb29F06D2e4', symbol='variableDebtEthwstETH')). 
User AaveV3User(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), address='0x0a38b2C1e86900Ea1Bb28a261E06582Ac9e9E386', e_mode=0) scaled balance (20459197) does not match contract balance (0) at block 22126931
```

## Root Cause

In `_collect_debt_burns`, the event type filter at lines 2040-2043 is too restrictive for GHO liquidations. When `is_gho=True`, the filter only allows `GHO_DEBT_BURN` events and skips all `DEBT_BURN` events, preventing secondary (non-GHO) debt burns from being collected.

### The Bug

```python
# Lines 2040-2043
# Filter by event type
if is_gho and ev.event_type != ScaledTokenEventType.GHO_DEBT_BURN:
    continue  # BUG: Skips ALL non-GHO debt burns, including secondary debt!
if not is_gho and ev.event_type != ScaledTokenEventType.DEBT_BURN:
    continue
```

### Event Sequence in Transaction

**Transaction:** 0xd693863b3e0e1ce42ac188acbb2a5f6457e1911315942b1ee4e28ec098fe4760
**Block:** 22126931
**Type:** Bad Debt GHO Liquidation with Multi-Asset Debt

| Log Index | Event | Contract | Details |
|-----------|-------|----------|---------|
| 341 | GHO_DEBT_BURN | GhoVariableDebtToken | user=0x0a38b2C1..., amount=9,657,650,934,004,283,397 |
| 346-349 | Collateral Transfers | aEthWETH | WETH collateral seized |
| 350 | Transfer | variableDebtEthwstETH | 20,647,496 wstETH to 0x0 |
| 351 | Burn | variableDebtEthwstETH | amount=20,638,769, balanceIncrease=8,727 |
| 357 | LiquidationCall | Pool | debtAsset=GHO, collateralAsset=WETH |

### What Happened

1. Operation 10 (GHO_LIQUIDATION) is created for the LiquidationCall at logIndex 357
2. `_collect_debt_burns` is called with `is_gho=True`
3. The loop encounters the wstETH `DEBT_BURN` at logIndex 351
4. The filter at line 2040-2041 skips this event because:
   - `is_gho=True`
   - `ev.event_type == ScaledTokenEventType.DEBT_BURN` (not GHO_DEBT_BURN)
5. The secondary debt burn is never added to the liquidation operation
6. The unassigned burn gets classified as `INTEREST_ACCRUAL` (Operation 116)
7. When processing INTEREST_ACCRUAL, the debt position is NOT cleared
8. Balance verification fails: local balance 20,459,197 vs on-chain balance 0

### The Problematic Flow

```
_collect_debt_burns(is_gho=True, ...):
    for ev in scaled_events:
        if is_gho and ev.event_type != GHO_DEBT_BURN:
            continue  # <-- Skips wstETH DEBT_BURN!
        
        # Lines 2079-2087 never reached for wstETH burn
        elif event_token_address != debt_v_token_address:
            # Secondary debt collection code - UNREACHABLE for GHO liquidations!
```

## Transaction Details

| Field | Value |
|-------|-------|
| **Hash** | 0xd693863b3e0e1ce42ac188acbb2a5f6457e1911315942b1ee4e28ec098fe4760 |
| **Block** | 22126931 |
| **Type** | Bad Debt GHO Liquidation with Multi-Asset Debt |
| **User** | 0x0a38b2C1e86900Ea1Bb28a261E06582Ac9e9E386 |
| **Primary Debt** | GHO (~9.66 GHO burned) |
| **Secondary Debt** | wstETH (20,638,769 + 8,727 = 20,647,496 scaled units) |
| **Collateral** | WETH (~0.0042 ETH seized) |
| **Pool Revision** | 7 |
| **vToken Revision (wstETH)** | 1 |

## Fix

**File:** `src/degenbot/cli/aave_transaction_operations.py`

**Location:** `_collect_debt_burns` function, lines 2039-2087

**Change:** Remove `is_gho` from event type filtering and consolidate all burn collection into a single path. This aligns with the 0051 intent to remove primary/secondary distinction.

```python
# BEFORE (buggy - has primary/secondary split despite 0051 claiming to remove it):
for ev in scaled_events:
    if ev.event["logIndex"] in assigned_indices:
        continue
    if ev.user_address != user:
        continue

    # Filter by event type
    if is_gho and ev.event_type != ScaledTokenEventType.GHO_DEBT_BURN:
        continue  # BUG: Skips non-GHO burns!
    if not is_gho and ev.event_type != ScaledTokenEventType.DEBT_BURN:
        continue

    event_token_address = get_checksum_address(ev.event["address"])

    # Check if this burn matches the liquidation's debt asset
    if debt_v_token_address is not None and event_token_address == debt_v_token_address:
        # This is a burn for the primary debt asset
        # ... multi-liquidation disambiguation ...
        burns.append(ev)
        assigned_indices.add(ev.event["logIndex"])

    elif debt_v_token_address is not None and event_token_address != debt_v_token_address:
        # This is a burn for a different debt asset (multi-asset liquidation)
        # ... secondary debt collection ...
        burns.append(ev)
        assigned_indices.add(ev.event["logIndex"])

# AFTER (fixed - single unified path):
for ev in scaled_events:
    if ev.event["logIndex"] in assigned_indices:
        continue
    if ev.user_address != user:
        continue

    # Accept any debt burn event type
    # Both GHO_DEBT_BURN and DEBT_BURN are valid for any liquidation
    if ev.event_type not in {
        ScaledTokenEventType.GHO_DEBT_BURN,
        ScaledTokenEventType.DEBT_BURN,
    }:
        continue

    event_token_address = get_checksum_address(ev.event["address"])

    # Multi-liquidation disambiguation: only skip burns that are clearly
    # for a different liquidation (same asset, much smaller amount)
    # See debug/aave/0051 for rationale
    if is_multi_liquidation and debt_to_cover > 0:
        if debt_v_token_address is not None and event_token_address == debt_v_token_address:
            total_burn = ev.amount + (ev.balance_increase or 0)
            if total_burn > debt_to_cover * 10:
                log_index = ev.event["logIndex"]
                logger.debug(
                    f"_collect_debt_burns: Skipping burn at "
                    f"logIndex {log_index} (total_burn={total_burn}) - "
                    f"more than 10x debtToCover ({debt_to_cover}), "
                    f"likely belongs to different liquidation."
                )
                continue

    # All other burns: accept unconditionally (handles both primary and secondary)
    burns.append(ev)
    assigned_indices.add(ev.event["logIndex"])
    if ev.index is not None and ev.index > 0:
        assigned_indices.add(ev.index)
```

**Key changes:**
1. **Remove `is_gho` from event type filter** - Both `GHO_DEBT_BURN` and `DEBT_BURN` are accepted for any liquidation
2. **Remove primary/secondary branch split** - All burns are collected via single path
3. **Simplify to semantic matching** - Match by user, validate by event type, apply disambiguation only when needed
4. **Consistent with 0051 intent** - The docstring promised "Collect ALL debt burns" but code didn't deliver

## Key Insight

**The 0051 refactoring was incomplete.**

The 0051 docstring states "Collect ALL debt burns" and claims to have "Removed primary/secondary distinction", but the code still had:
1. An `is_gho`-based event type filter that split GHO vs non-GHO burns
2. Separate `if/elif` branches for "primary" vs "secondary" debt

This partial refactoring created the bug: the event type filter ran before the code could determine which branch an event belonged to, causing non-GHO burns to be skipped in GHO liquidations.

**The fix completes the 0051 refactoring:**
- Remove `is_gho` from the filter entirely
- Remove the primary/secondary branch distinction
- Accept any debt burn event type unconditionally
- Apply disambiguation only for multi-liquidation scenarios (same user/asset)

**Architectural lesson:** When refactoring to remove a distinction, ensure all code paths that relied on that distinction are updated. Partial refactoring leaves behind bugs that are hard to detect because the code contradicts its documentation.

## Verification

**Test command:**
```bash
uv run degenbot aave update
```

**Result:** PASSED

```
AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True) successfully updated to block 22,126,931
```

The wstETH debt burn at logIndex 351 is now properly collected and processed. Both GHO and wstETH debt positions for user 0x0a38b2C1e86900Ea1Bb28a261E06582Ac9e9E386 were cleared during the bad debt liquidation, and balance verification passed.

## Refactoring

The fix in this issue **completes the 0051 refactoring**:

1. **Removed `is_gho` parameter** from `_collect_debt_burns` - no longer needed
2. **Unified event type filter** - accepts both `GHO_DEBT_BURN` and `DEBT_BURN` 
3. **Removed primary/secondary branch split** - single path collects all burns
4. **Function now matches its stated intent:** "Collect ALL debt burns for the liquidated user"

## Related Issues

- Issue 0051: Bad Debt Liquidation Debt Burn Matching Uses Only Principal Amount (fixed ratio threshold)
- Issue 0029: Multi-Asset Liquidation Missing Secondary Debt Burns Fix (introduced secondary burn logic)
- Issue 0028: Multi-Asset Liquidation Missing Secondary Debt Burns

## References

- Contract: `contract_reference/aave/VariableDebtToken/rev_1.sol` (Burn event definition)
- Contract: `contract_reference/aave/GhoVariableDebtToken/rev_3.sol` (GHO Burn event definition)
- File: `src/degenbot/cli/aave_transaction_operations.py` (lines 2039-2087)
- Transaction: 0xd693863b3e0e1ce42ac188acbb2a5f6457e1911315942b1ee4e28ec098fe4760
- Investigation report: `/tmp/aave_v3_liquidation_report.md`
