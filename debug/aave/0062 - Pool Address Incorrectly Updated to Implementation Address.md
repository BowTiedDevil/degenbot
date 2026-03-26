# Issue 0062: Pool Address Incorrectly Updated to Implementation Address

**Date:** 2026-03-26

## Issue ID
0062

## Symptom

Transaction validation failure at block 23081071:
```
Transaction validation failed:
1 scaled token events (Burn/Mint/BalanceTransfer) unassigned: [153]. 
DEBUG NOTE: All scaled token events must be matched to operations. 
Unassigned burns/mints/transfers indicate a matching bug.
```

**Key Debug Output:**
```
Updated address for POOL to 0x97287a4F35E583D924f78AD88DB8AFcE1379189A
...
DEBUG: Fetched 0 pool events
...
Transaction Hash: 0xef13c582f435d735866ceba8d70f7e0bcd935f93e9ccbd4e58fe177476df21a8
Block: 23081071
PARSED OPERATIONS (0)
```

## Transaction Details

- **Transaction Hash:** 0xef13c582f435d735866ceba8d70f7e0bcd935f93e9ccbd4e58fe177476df21a8
- **Block:** 23081071
- **Market:** Aave Ethereum Market
- **User:** 0x1C5daa0e35f0d8378f7c79F8C8F3FCb8Ed3B5856
- **Operation:** BORROW (300 USDT at variable rate)
- **Asset:** USDT (variableDebtEthUSDT at 0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8)

## On-Chain Verification

**Actual Pool Proxy Address (constant):**
```bash
cast call 0x2f39d218133afab8f2b819b1066c7e434ad94e9e "getPool()" \
  --block 23081071 --rpc-url http://node:8545
# Result: 0x00000000000000000000000087870bca3f3fd6335c3f4ce8392d69350b4fa4e2
```

**Pool Events DO exist at the proxy address:**
```bash
cast logs --from-block 23081071 --to-block 23081100 \
  --address 0x87870bca3f3fd6335c3f4ce8392d69350b4fa4e2 \
  --rpc-url http://node:8545 | wc -l
# Result: 1658 events found
```

**Events in the failing transaction (from cast receipt):**
- `[152] Transfer` - variableDebtEthUSDT debt tokens minted to user
- `[153] Mint` - VariableDebtToken Mint event (UNMATCHED - the issue)
- `[154] ReserveDataUpdated` - Pool contract event
- `[155] Transfer` - USDT transferred from aToken to borrower
- `[156] Borrow` - Pool Borrow event (MISSING from processing!)

## Root Cause Analysis

### The Problem: Pool Address Incorrectly Updated

When processing `POOL_UPDATED` events from the PoolAddressesProvider, the code incorrectly updates the Pool contract address to the **implementation address** instead of keeping the **proxy address**.

**POOL_UPDATED Event Structure:**
```solidity
event PoolUpdated(address indexed oldAddress, address indexed newAddress);
```
- `topics[1]`: Old implementation address
- `topics[2]`: New implementation address

**The Proxy Pattern:**
- **Pool Proxy Address:** 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 (NEVER changes)
- **Implementation Address:** Can be upgraded via proxy pattern (currently 0x97287a4F35E583D924f78AD88DB8AFcE1379189A)

### Where the Bug Occurs

**File:** `src/degenbot/cli/aave.py`
**Lines:** 4805-4813 (Phase 1)

```python
elif topic == AaveV3PoolConfigEvent.POOL_UPDATED.value:
    # Capture address change but defer revision update
    _update_contract_address_only(
        session=session,
        market=market,
        contract_name="POOL",
        new_address=decode_address(event["topics"][2]),  # BUG!
    )
    proxy_events.append(event)
```

### The Impact

1. **Phase 1:** Pool address incorrectly updated from proxy to implementation
2. **Phase 3:** Pool events fetched using wrong address (implementation instead of proxy)
3. **Result:** "Fetched 0 pool events" because events are emitted by the PROXY
4. **Transaction Processing:** BORROW operations cannot be created without Borrow events
5. **Validation Failure:** DEBT_MINT event has no matching BORROW operation

## The Fix

### Solution: Don't Update Pool Address on POOL_UPDATED

The Pool proxy address should NEVER change. Only the implementation address changes.

**File:** `src/degenbot/cli/aave.py`
**Lines:** 4805-4813

**Remove the `_update_contract_address_only` call:**

```python
elif topic == AaveV3PoolConfigEvent.POOL_UPDATED.value:
    # Pool proxy address NEVER changes - only the implementation changes
    # Do NOT update the address - just save event for revision update
    proxy_events.append(event)
```

Similarly for POOL_CONFIGURATOR_UPDATED (lines 4814-4823).

## Key Insight

**Proxy contract addresses are immutable.**

In the Aave proxy pattern:
1. The PoolAddressesProvider stores the Pool proxy address
2. The Pool proxy stores the implementation address (upgradeable)
3. Events are ALWAYS emitted from the proxy contract
4. POOL_UPDATED signals implementation upgrade, NOT proxy address change

## Related Issues

- Issue 0061: Pool Revision Upgrade Timing Error

Both issues relate to Pool upgrade handling:
- Issue 0061: Revision applied to entire block range
- Issue 0062: Pool address updated to implementation address

## Summary

The Pool contract address was incorrectly updated to the implementation address (0x97287a4F...) when processing a POOL_UPDATED event. The actual Pool proxy address (0x87870Bca...) should never change.

When Phase 3 fetched Pool events using the wrong address, it retrieved 0 events. This caused the BORROW operation to be missed, leaving the DEBT_MINT event unmatched.

**Fix:** Do NOT update Pool address when processing POOL_UPDATED events. Only update the revision when processed chronologically in Phase 3.

## Status: 🟢 FIXED

**Applied in:** `src/degenbot/cli/aave.py`

**Changes Made:**
1. Removed `_update_contract_address_only()` call for POOL_UPDATED events (lines 4805-4813)
2. Removed `_update_contract_address_only()` call for POOL_CONFIGURATOR_UPDATED events (lines 4815-4823)
3. Added clarifying comments that proxy addresses never change

**Verification:**
- Pool address remains at 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2
- Pool events fetched correctly (1658+ events)
- Transaction 0xef13c582... processes with BORROW operation
- Debt mint event properly matched
