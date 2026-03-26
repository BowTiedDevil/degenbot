# Issue 0061: Pool Revision Upgrade Timing Error

**Date:** 2026-03-26

## Issue ID
0061

## Symptom

Balance verification failure at block 23088120:
```
AssertionError: Collateral balance verification failure for AaveV3Asset(..., symbol='WETH').
User AaveV3User(..., address='0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c')
scaled balance (4872543720628382936660) does not match contract balance (4872543720628382936659) at block 23088120

Difference: 1 wei
```

The calculated balance is **1 wei higher** than the actual contract balance.

## Transaction Details

- **Transaction Hash:** 0x6c29cd8718df10f2289e86f9c4de995defdf7a4e3e77c10836d3d60296075859
- **Block:** 23088120
- **Market:** Aave Ethereum Market
- **User:** 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c (Aave Treasury)
- **Asset:** WETH (aEthWETH at 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8)
- **Operation:** MINT_TO_TREASURY (part of batch mintToTreasury for 29 assets)
- **Pool Contract Revision at block 23088120:** 8
- **aToken Revision:** 3

## On-Chain Verification

```bash
# Query contract revision at block 23088120
cast call 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 "POOL_REVISION()" --block 23088120 --rpc-url http://node:8545
# Result: 0x0000000000000000000000000000000000000000000000000000000000000008

# Query actual scaled balance at block 23088120
cast call 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8 "scaledBalanceOf(address)" \
  0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c --block 23088120 --rpc-url http://node:8545
# Result: 0x0000000000000000000000000000000000000000000001082420ba1186f12e53
# = 4872543720628382936659
```

## Root Cause Analysis

**IMPORTANT:** During investigation, a SECOND issue was discovered (Issue 0062: Pool Address Incorrectly Updated to Implementation Address) that was the actual root cause of the validation failures. This report documents the revision timing issue, but the fix applied addressed Issue 0062.

### The Problem: Revision Detection Timing

The update process detected Pool revision 9 at the **start** of the block range 23,081,071 and upgraded the revision in the database immediately:

```
Processing block range 23,081,071 -> 23,091,070:  0.0% |          |Upgraded revision for POOL to 9
```

**This is incorrect.** The actual Pool contract implementation upgrade happened at a **later block** within the range. Block 23088120 still had Pool revision 8.

### The Impact on MINT_TO_TREASURY

The code at `src/degenbot/aave/libraries/pool_math.py` uses different rounding modes based on Pool revision:

- **Pool Rev 1-8:** Uses `ray_div` (half-up rounding)
- **Pool Rev 9+:** Uses `ray_div_ceil` (ceiling rounding)

When processing the MINT_TO_TREASURY operation at block 23088120, the code incorrectly used Rev 9 math (`ray_div_ceil`) when it should have used Rev 8 math (`ray_div`).

### Mathematical Verification

**Mint Event Data:**
- Mint value: 112985924809412137157 (underlying units)
- Balance increase (interest): 172022732247172231
- Actual mint amount: 112813902077164964926
- Liquidity index: 1050525151551449303099972196

**Calculations:**
```python
# Pool Rev 8 (correct for block 23088120)
scaled_half_up = (actual_mint * RAY + index // 2) // index
# Result: 107388101951255302162

# Pool Rev 9+ (incorrectly used)
scaled_ceil = (actual_mint * RAY + index - 1) // index
# Result: 107388101951255302163

# Difference: 1 wei
```

The code added 1 wei more than it should have, resulting in the verification failure.

### Why This Happens

The revision detection logic queries the current Pool revision at the start of processing a block range. When the Pool contract is upgraded via a proxy pattern:

1. A transaction at block N calls `upgradeToAndCall()` on the proxy
2. The implementation address changes
3. The new implementation has a higher revision number
4. The update process detects this immediately
5. It applies the new revision to ALL blocks in the range, including those before the upgrade

This is a **fundamental architectural issue**: revision changes are applied to the entire block range instead of only to blocks at or after the upgrade transaction.

### Actual Root Cause (Issue 0062)

During investigation, validation errors revealed that **Pool events were not being fetched** (0 pool events), causing BORROW operations to fail. The actual root cause was that the Pool address was being incorrectly updated to the **implementation address** instead of keeping the **proxy address** when processing `POOL_UPDATED` events.

**See:** [Issue 0062 - Pool Address Incorrectly Updated to Implementation Address](0062%20-%20Pool%20Address%20Incorrectly%20Updated%20to%20Implementation%20Address.md)

The proxy pattern means:
- **Proxy address (0x87870Bca...):** NEVER changes, emits all events
- **Implementation address:** Changes with upgrades, emits NO events

When the Pool address was updated to the implementation, `_fetch_pool_events` found 0 events, breaking all operation matching.

## The Fix

**Note:** The fix that resolved the immediate validation failures was Issue 0062 (Pool Address Incorrectly Updated to Implementation Address). The changes below document the chronological processing approach developed in this issue, which properly handles revision upgrades but was not the root cause fix.

### Implementation: Defer Pool Upgrade Processing Until Chronological Event Processing

The approach modifies `update_aave_market` to defer Pool revision updates until the actual upgrade event is processed chronologically within the transaction flow.

**Changes Made:**

1. **Modified Phase 1** to collect proxy upgrade events without immediately updating revisions:
   - Added `proxy_events` list to collect POOL_UPDATED and POOL_CONFIGURATOR_UPDATED events
   - Removed address updates (see Issue 0062 - Pool proxy address NEVER changes)
   - Events are saved for chronological processing in Phase 3

2. **Modified Phase 3** to include proxy events in chronological processing:
   - Added `proxy_events` to `all_events` before sorting
   - This ensures upgrade events are processed at the correct block position

3. **Added event handlers in `_process_transaction()`** to process upgrades chronologically:
   - Added handler for `AaveV3PoolConfigEvent.POOL_UPDATED`
   - Added handler for `AaveV3PoolConfigEvent.POOL_CONFIGURATOR_UPDATED`
   - When processed, updates the revision AND updates `tx_context.pool_revision`
   - This ensures subsequent operations in the same transaction use the correct revision

**Key Code Changes:**

**Phase 1 - Collect instead of process:**
```python
# Phase 1: Collect proxy events
proxy_events: list[LogReceipt] = []

for event in _fetch_address_provider_events(...):
    topic = event["topics"][0]
    
    if topic == AaveV3PoolConfigEvent.POOL_UPDATED.value:
        # Pool proxy address NEVER changes - only implementation changes
        # Save event for revision update in Phase 3
        proxy_events.append(event)
```

**Phase 3 - Include proxy events:**
```python
# Phase 3
all_events: list[LogReceipt] = []
all_events.extend(proxy_events)  # Add proxy events for chronological processing
# ... rest of event fetching ...
```

**Transaction processing - Handle upgrades chronologically:**
```python
elif topic == AaveV3PoolConfigEvent.POOL_UPDATED.value:
    # Process Pool upgrade chronologically at the correct block
    new_revision = _update_contract_revision(...)
    # Update the transaction context with the new revision
    tx_context.pool_revision = new_revision
    logger.info(
        f"Pool upgraded to revision {new_revision} at block {event['blockNumber']} "
        f"(transaction {event['transactionHash'].to_0x_hex()})"
    )
```

### Actual Fix Applied (Issue 0062)

The root cause fix removed the incorrect address update:

```python
elif topic == AaveV3PoolConfigEvent.POOL_UPDATED.value:
    # Pool proxy address NEVER changes - only the implementation changes
    # Save event for chronological processing in Phase 3
    # The revision will be updated when the event is processed chronologically
    proxy_events.append(event)
```

**Key Difference:** The POOL_UPDATED event provides the **implementation address** in `topics[2]`, not a new proxy address. The Pool proxy address (0x87870Bca...) remains constant.

**Phase 3 - Include proxy events:**
```python
# Phase 3
all_events: list[LogReceipt] = []
all_events.extend(proxy_events)  # Add proxy events for chronological processing
# ... rest of event fetching ...
```

**Transaction processing - Handle upgrades chronologically:**
```python
elif topic == AaveV3PoolConfigEvent.POOL_UPDATED.value:
    # Process Pool upgrade chronologically at the correct block
    new_revision = _update_contract_revision(...)
    # Update the transaction context with the new revision
    tx_context.pool_revision = new_revision
    logger.info(
        f"Pool upgraded to revision {new_revision} at block {event['blockNumber']} "
        f"(transaction {event['transactionHash'].to_0x_hex()})"
    )
```
```

**File:** `src/degenbot/cli/aave.py`

Get revision for a specific block:
```python
def _get_pool_revision_for_block(
    *,
    session: Session,
    market: AaveV3Market,
    block_number: int,
) -> int:
    """Get the Pool revision effective at a specific block."""
    contract = _get_contract(session=session, market=market, contract_name="POOL")
    
    # If we have a recorded upgrade block and the query block is before it
    if contract.revision_change_block is not None and block_number < contract.revision_change_block:
        # Query on-chain for the revision at that specific block
        # This handles the initial state before our recorded history
        (revision,) = raw_call(
            w3=w3,
            address=contract.address,
            calldata=encode_function_calldata(
                function_prototype="POOL_REVISION()",
                function_arguments=None,
            ),
            return_types=["uint256"],
            block_identifier=block_number,
        )
        return revision
    
    return contract.revision
```

**File:** `src/degenbot/cli/aave.py`

Use block-specific revision when processing operations:
```python
def _process_mint_to_treasury(
    *,
    operation: Operation,
    tx_context: TransactionContext,
) -> None:
    # Get the revision effective at this transaction's block
    pool_revision = _get_pool_revision_for_block(
        session=tx_context.session,
        market=tx_context.market,
        block_number=operation.scaled_token_events[0].event["blockNumber"],
    )
    
    # Use block-specific revision for calculations
    scaled_amount = _calculate_mint_to_treasury_scaled_amount(
        scaled_event=scaled_event,
        operation=operation,
        pool_revision=pool_revision,  # Pass explicit revision
    )
```

### Option 2: Query On-Chain Per Transaction

For each transaction, query the Pool revision at that specific block before processing. This is simpler but adds RPC overhead.

### Option 3: Store Historical Revisions

Maintain a full revision history table:

```python
class AaveV3ContractRevision(Base):
    """Historical record of contract revisions."""

    contract_id: Mapped[int] = mapped_column(ForeignKey("aave_v3_contracts.id"))
    revision: Mapped[int]
    start_block: Mapped[int]
    end_block: Mapped[int | None]  # NULL means current
```

Query the appropriate revision for each transaction.

## Recommended Solution

**Implement Option 1** with the following rationale:

1. **Minimal overhead:** Only requires one additional RPC call when a revision upgrade is detected
2. **Backward compatible:** Existing processing continues to work
3. **Precise:** Uses the exact revision effective at each block
4. **Maintainable:** Clear separation between revision detection and usage

## Key Insight

**Contract revisions can change mid-range.**

When processing a block range:
1. Don't assume the revision is constant for the entire range
2. Record the upgrade block number when a proxy upgrade is detected
3. Always use the revision effective at the transaction's block
4. This affects ALL operations that use revision-specific math (not just MINT_TO_TREASURY)

This is similar to how block timestamps or other on-chain parameters can change within a range - the revision is part of the on-chain state and must be queried at the specific block being processed.

## Related Issues

- **Issue 0062: Pool Address Incorrectly Updated to Implementation Address** - The ACTUAL root cause of the validation failures
- Issue 0034: MINT_TO_TREASURY Cumulative Rounding Error Exceeds Tolerance
- Issue 0036: MINT_TO_TREASURY Pool Revision 8 Rounding Error
- Issue 0049: SUPPLY Collateral Mint Uses Mint Event Amount Instead of Supply Amount

**Note:** While Issue 0061 documents the revision timing problem, Issue 0062 was the actual root cause preventing the update from processing. The validation errors (unassigned Mint events) were caused by Pool events not being fetched due to incorrect Pool address, not by revision mismatch.

## References

- Transaction: 0x6c29cd8718df10f2289e86f9c4de995defdf7a4e3e77c10836d3d60296075859
- Block: 23088120
- Treasury: 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c
- aWETH Token: 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8
- Pool Proxy: 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2
- Pool Revision at block 23088120: 8
- Update detected Pool Revision 9 at start of range 23,081,071

## Files Involved

- `src/degenbot/cli/aave.py` - Main processing logic and revision detection
- `src/degenbot/aave/libraries/pool_math.py` - Revision-aware calculations
- `src/degenbot/database/models/aave.py` - Database models

## Summary

The failure occurred because the update process detected Pool revision 9 at the start of the block range and applied Rev 9 math to all transactions in that range. However, block 23088120 still had Pool revision 8, which uses different rounding in MINT_TO_TREASURY calculations.

The calculated scaled amount was 1 wei higher than the actual contract balance because:
- Rev 9 uses `ray_div_ceil` (ceiling rounding)
- Rev 8 uses `ray_div` (half-up rounding)
- The ceiling operation added 1 wei compared to half-up

**The fix requires tracking revision upgrades with their block numbers and using the appropriate revision for each transaction based on its block.**

## Status: 🟢 RESOLVED via Issue 0062

**Resolution:** The validation failures that prompted this investigation were actually caused by Issue 0062 (Pool Address Incorrectly Updated to Implementation Address), not by the revision timing issue documented here.

**Applied Fix (in Issue 0062):**
- Removed `_update_contract_address_only()` call for POOL_UPDATED events
- Pool proxy address now correctly remains at 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2
- Pool events now fetched correctly from proxy address

**Chronological Processing (this issue's contribution):**
- Modified Phase 1 to collect POOL_UPDATED events instead of processing immediately
- Added proxy events to Phase 3 chronological processing
- Added handlers in `_process_transaction` to update Pool revision at the correct block

**Note on Revision Timing:** While the revision timing issue documented in this report is a valid architectural concern, it was NOT the root cause of the observed failures. The 1 wei discrepancy would have occurred, but the validation errors were caused by missing Pool events due to Issue 0062.

**Future Work:** The chronological revision update approach developed in this issue remains valuable for properly handling mid-range revision upgrades, even though it wasn't the immediate fix needed.
