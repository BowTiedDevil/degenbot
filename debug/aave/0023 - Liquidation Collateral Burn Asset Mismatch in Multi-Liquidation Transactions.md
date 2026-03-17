# Issue 0023: Liquidation Collateral Burn Asset Mismatch in Multi-Liquidation Transactions

**Date:** 2026-03-17  
**Status:** ✅ FIXED

---

## Symptom

```
AssertionError: Balance verification failure for AaveV3Asset(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), underlying_token=Erc20TokenTable(chain=1, address='0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2', symbol=None), a_token=Erc20TokenTable(chain=1, address='0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8', symbol=None), v_token=Erc20TokenTable(chain=1, address='0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE', symbol=None)). User AaveV3User(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), address='0xb24864C391f454CFc6135E4D93814C420eAb797d', e_mode=0) scaled balance (15819238335110273148) does not match contract balance (13765444704282452384) at block 20459337
```

**Balance Discrepancy:**
- Expected (calculated): 15.819238335110273148 WETH
- Actual (on-chain): 13.765444704282452384 WETH
- Difference: 2.053793630827820764 WETH (~2.05 WETH)

---

## Root Cause

When a user is liquidated **multiple times in the same transaction** with **different collateral assets**, the collateral burn events are being matched to the **wrong liquidation operation**.

### Transaction Analysis

**Transaction:** `0x21812980c660d1a7e63c1f09b0a01e6d5d01301769ad341104aeed0b7718e3c0`
**Block:** 20459337

User `0xb24864c391f454cfc6135e4d93814c420eab797d` was liquidated **TWICE** in this transaction:

1. **First Liquidation** (Operation 2, Pool event at logIndex=112):
   - **Collateral Asset:** USDC
   - **Debt Asset:** USDC  
   - **Collateral Burn:** LogIndex 209
   - **Result:** Processed correctly

2. **Second Liquidation** (Operation 8, Pool event at logIndex=214):
   - **Collateral Asset:** WETH
   - **Debt Asset:** WETH
   - **Collateral Burn:** LogIndex 105
   - **Expected Burn Amount:** ~2.10 WETH
   - **Actual Processing:** Burn was applied to **USDC position instead of WETH position**

### The Bug

In `_create_liquidation_operation()` (aave_transaction_operations.py:1756-1865), when finding collateral burn events for a liquidation, the code only matches on `user_address` and `event_type`:

```python
collateral_burn: ScaledTokenEvent | None = None
for ev in scaled_events:
    if ev.event["logIndex"] in assigned_indices:
        continue
    if ev.event_type == ScaledTokenEventType.COLLATERAL_BURN:
        if ev.user_address == user:
            collateral_burn = ev
```

**The Problem:** When a user is liquidated multiple times with different collateral assets, the collateral burn events from different liquidations are interleaved in the log. The matching logic doesn't verify that the **token contract address** of the burn event matches the **collateral asset** of the liquidation.

### Event Sequence

```
LogIndex 101: DEBT_BURN (USDC debt for Operation 2)
LogIndex 104: ERC20_COLLATERAL_TRANSFER (USDC for Operation 2)
LogIndex 105: COLLATERAL_BURN (WETH for Operation 8) <-- WRONGFULLY MATCHED
LogIndex 109: ERC20_COLLATERAL_TRANSFER (USDC for Operation 2)
LogIndex 112: LIQUIDATION_CALL (Operation 2 pool event)
...
LogIndex 209: COLLATERAL_BURN (USDC for Operation 2)
LogIndex 214: LIQUIDATION_CALL (Operation 8 pool event)
```

When creating Operation 2, the code found the COLLATERAL_BURN at logIndex=105 (which belongs to Operation 8's WETH liquidation) and assigned it to Operation 2's USDC liquidation because it matched only on user address.

### Processing Evidence

Operation 8 processing log shows:
```
Processing _process_collateral_burn_with_match at block 20459337
Processing scaled token operation (CollateralBurnEvent) for revision 1
AaveV3CollateralPosition(..., underlying_token=0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48, ...)  <-- USDC!
_process_scaled_token_operation burn: delta=-1953952828149791001, new_balance=-1953952828081539711
```

The WETH collateral burn (amount ~2.10 WETH) was incorrectly processed against the USDC collateral position, causing:
1. USDC position went negative (which should never happen)
2. WETH position retained the 2.10 WETH that should have been burned
3. Balance verification failed for WETH

---

## Transaction Details

| Field | Value |
|-------|-------|
| **Transaction Hash** | 0x21812980c660d1a7e63c1f09b0a01e6d5d01301769ad341104aeed0b7718e3c0 |
| **Block Number** | 20459337 |
| **Timestamp** | 2024-08-04 06:41:43 UTC |
| **User Liquidated** | 0xb24864c391f454cfc6135e4d93814c420eab797d |
| **Liquidator** | 0x00000000009E50a7dDb7a7B0e2ee6604fd120E49 |
| **Pool Revision** | 4 |
| **aToken Revision** | 1 |
| **vToken Revision** | 1 |

### Contract Addresses

| Contract | Address |
|----------|---------|
| Aave V3 Pool (Proxy) | 0x87870BCA3F3fd6335C3F4Ce8392D69350B4fA4E2 |
| aWETH (Proxy) | 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8 |
| vWETH (Proxy) | 0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE |

---

## Fix

**Status:** ✅ IMPLEMENTED AND TESTED

**Files Modified:**
- `src/degenbot/cli/aave_transaction_operations.py`

**Functions Modified:**
- `_create_liquidation_operation()` (lines 1756-1902)

**Functions Added:**
- `_get_a_token_for_asset()` (lines 537-561)
- `_get_v_token_for_asset()` (lines 563-587)

### Implementation Details

The fix adds token address validation when matching liquidation events. Two new helper methods were added to query the aToken and vToken contract addresses for underlying assets from the database.

#### Helper Methods Added:

```python
def _get_a_token_for_asset(
    self, underlying_asset: ChecksumAddress
) -> ChecksumAddress | None:
    """Get the aToken address for an underlying asset."""
    # Queries database to find aToken for given underlying asset
    
def _get_v_token_for_asset(
    self, underlying_asset: ChecksumAddress
) -> ChecksumAddress | None:
    """Get the vToken address for an underlying asset."""
    # Queries database to find vToken for given underlying asset
```

#### Modified Event Matching Logic:

**Before:** Events matched only by `user_address` and `event_type`
```python
if ev.event_type == ScaledTokenEventType.COLLATERAL_BURN:
    if ev.user_address == user:
        collateral_burn = ev
```

**After:** Events matched by `user_address`, `event_type`, AND `token_contract_address`
```python
# Get token contract addresses for the collateral and debt assets
collateral_a_token_address = self._get_a_token_for_asset(_collateral_asset)
debt_v_token_address = self._get_v_token_for_asset(debt_asset)

# Match collateral events only if they belong to this liquidation's collateral asset
event_token_address = get_checksum_address(ev.event["address"])
if (
    collateral_a_token_address is not None
    and event_token_address != collateral_a_token_address
):
    continue

if (
    ev.event_type == ScaledTokenEventType.COLLATERAL_BURN
    and ev.user_address == user
):
    collateral_burn = ev
```

#### Same Fix Applied to Debt Events:

```python
# Match debt burn events only if they belong to this liquidation's debt asset
event_token_address = get_checksum_address(ev.event["address"])
if debt_v_token_address is not None and event_token_address == debt_v_token_address:
    debt_burn = ev
    break
```

### Testing

The fix was verified by running the Aave update on the previously failing block:

```bash
uv run degenbot aave update --chunk 1
```

**Result:** ✅ Successfully processed block 20459337 without balance verification errors.

### Additional Changes

- Combined nested if statements to satisfy linter requirements (SIM102)
- Added comprehensive docstrings for new helper methods

---

## Key Insight

**FUNDAMENTAL PREMISE CONFIRMED:** The on-chain contract state is correct. The balance verification failure is caused by the processing code incorrectly matching events to operations.

When a user is liquidated multiple times in a single transaction with different collateral assets, the event matching must be **asset-specific**, not just **user-specific**. The current implementation assumes that within a transaction, a user will only be liquidated once per collateral asset, which is not always true.

---

## Refactoring

### Completed:
1. ✅ **Event Matching Enhancement:** Added token address validation to liquidation event matching logic
2. ✅ **Helper Methods:** Added `_get_a_token_for_asset()` and `_get_v_token_for_asset()` for token address lookups
3. ✅ **Code Quality:** Combined nested if statements to satisfy linter requirements

### Recommended Future Work:
3. **Test Coverage:** Create test cases for multi-liquidation transactions where a user is liquidated multiple times with different collateral assets in the same transaction
4. **Documentation:** Update AGENTS.md with guidance on multi-liquidation event matching
5. **Audit Other Operations:** Review `_create_deficit_operation()` and other operation creation methods for similar token address matching issues

---

## References

- Transaction: https://etherscan.io/tx/0x21812980c660d1a7e63c1f09b0a01e6d5d01301769ad341104aeed0b7718e3c0
- EVM Investigation Report: `/tmp/aave_investigation_0023.txt`
- Processing Log: `debug/aave/0023_investigation.log`
- Related Issue: Similar event matching issues may exist in `_create_deficit_operation()` and other operation creation methods

---

## Summary

This issue was caused by the event matching logic in `_create_liquidation_operation()` not verifying that scaled token events belonged to the correct asset when a user was liquidated multiple times with different collateral assets in the same transaction.

The fix adds token contract address validation to ensure:
1. Collateral burn/transfer events match the liquidation's collateral asset
2. Debt burn events match the liquidation's debt asset

Two new database helper methods were added to lookup aToken and vToken contract addresses for underlying assets, enabling proper event-to-asset matching.

**Impact:** This fix resolves balance verification failures for users who are liquidated multiple times in the same transaction with different collateral assets, ensuring accurate position tracking in the database.

**Verification:** The fix was tested on block 20459337, which previously failed with a ~2.05 WETH balance discrepancy. After the fix, the block processed successfully without errors.
