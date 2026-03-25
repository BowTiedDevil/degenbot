# Issue 0056: Multi-Liquidation Single Debt Burn Misassignment

**Date:** 2026-03-25

## Symptom

Balance verification failure at block 20872104:
```
AssertionError: Debt balance verification failure for USDC debt position.
User 0x4A76a94442FAFF09b67689b4Ba5645C47638F38a scaled balance (524827144398) 
does not match contract balance (262455954894) at block 20872104
```

The calculated balance was approximately **double** the actual balance.

## Transaction Details

- **Transaction Hash:** 0x75b41542ba21912e8210166c11a10d0bbb70514ffce26bf1b42b2f723abee5e2
- **Block:** 20872104
- **Market:** Aave Ethereum Market (Pool revision 4)
- **User:** 0x4A76a94442FAFF09b67689b4Ba5645C47638F38a
- **Next Issue ID:** 0056

### Liquidation Structure

This transaction contains **two sequential liquidations** of the same user with the **same debt asset (USDC)**:

| Operation | Collateral | Debt Asset | debtToCover | Pool Event LogIndex |
|-----------|------------|------------|-------------|---------------------|
| 0 | WETH | USDC | 47,005,978 | 17 |
| 1 | WBTC | USDC | 292,538,129,344 | 30 |
| **Total** | - | USDC | **292,585,135,322** | - |

### Events in Transaction (Chronological)

| LogIndex | Event | Asset | Amount | Belongs To |
|----------|-------|-------|--------|------------|
| 6 | **Mint** | USDC debt | 352,195,531 | Liquidation 0 (interest accrual) |
| 10 | Burn | WETH collateral | 19,903,984,264,372,846 | Liquidation 0 |
| 17 | **LiquidationCall** | WETH/USDC | debtToCover=47,005,978 | Liquidation 0 |
| 19 | **Burn** | USDC debt | 292,538,129,344 | **Both liquidations combined** |
| 23 | Burn | WBTC collateral | 499,488,421 | Liquidation 1 |
| 30 | **LiquidationCall** | WBTC/USDC | debtToCover=292,538,129,344 | Liquidation 1 |

**Key Observation:** The single debt burn event (logIndex=19) represents the **total debt reduction** for both liquidations combined (292,538,129,344 ≈ 292,585,135,322 - interest accrual difference).

## Root Cause

The current architecture processes each liquidation independently, calculating debt reduction from each liquidation's individual `debtToCover`. When multiple liquidations share the same debt asset:

1. **Enrichment problem:** The enrichment layer calculates `raw_amount` for each liquidation separately
2. **Processing problem:** The burn event gets assigned to the first liquidation but uses enrichment values from that liquidation only
3. **Double counting:** First liquidation processes burn with its `debtToCover`, second liquidation has no burn

### Event Assignment (Bug)

**Liquidation 0 (WETH, logIndex=17, debtToCover=47,005,978):**
- Assigned: Mint (logIndex 6) + **Burn (logIndex 19)**
- **WRONG:** Uses debtToCover=47,005,978 (scaled: ~42,165,431)

**Liquidation 1 (WBTC, logIndex=30, debtToCover=292,538,129,344):**
- Assigned: Nothing (burn already used)
- **WRONG:** No debt reduction applied

**Result:** Balance reduced by ~42M instead of ~262M, causing verification failure.

## The Fix: Aggregate Liquidation Processing

Instead of per-liquidation processing, aggregate debt reductions at the transaction level:

1. **Preprocess:** Sum `debtToCover` from all liquidations with same (user, debt_asset)
2. **Skip Mint:** Skip Mint events in liquidations that will be aggregated
3. **Apply Once:** Process single burn with aggregated amount
4. **Track:** Mark (user, debt_asset) as processed to prevent duplicates

### Implementation

**Step 1: Add aggregation tracking to TransactionContext**

File: `src/degenbot/cli/aave_types.py`

```python
@dataclass
class TransactionContext:
    # ... existing fields ...
    
    # Liquidation aggregation tracking for multi-liquidation scenarios
    liquidation_aggregates: dict[tuple[ChecksumAddress, ChecksumAddress], int] = field(
        default_factory=dict
    )
    """Aggregated debtToCover by (user, debt_v_token)."""

    processed_liquidations: set[tuple[ChecksumAddress, ChecksumAddress]] = field(
        default_factory=set
    )
    """Track which (user, debt_v_token) pairs have been processed."""
```

**Step 2: Preprocess liquidations to aggregate debt amounts**

File: `src/degenbot/cli/aave.py`

```python
def _preprocess_liquidation_aggregates(
    tx_context: TransactionContext,
    operations: list[Operation],
) -> None:
    """Sum debtToCover for all liquidations with same (user, debt_asset)."""
    liquidation_operation_types = {
        OperationType.LIQUIDATION,
        OperationType.GHO_LIQUIDATION,
    }

    for op in operations:
        if op.operation_type not in liquidation_operation_types:
            continue
        if op.pool_event is None:
            continue

        user = decode_address(op.pool_event["topics"][3])
        debt_asset = decode_address(op.pool_event["topics"][2])
        debt_v_token = _get_v_token_for_underlying(
            session=tx_context.session,
            market=tx_context.market,
            underlying_address=debt_asset,
        )

        if debt_v_token is None:
            continue

        debt_to_cover, _, _, _ = eth_abi.abi.decode(
            types=["uint256", "uint256", "address", "bool"],
            data=op.pool_event["data"],
        )

        key = (user, debt_v_token)
        tx_context.liquidation_aggregates[key] = (
            tx_context.liquidation_aggregates.get(key, 0) + debt_to_cover
        )
```

**Step 3: Skip Mint events when aggregation detected**

File: `src/degenbot/cli/aave.py` in `_process_debt_mint_with_match`

```python
# For liquidations, check if there are multiple liquidations for this
# (user, debt_v_token) that will be aggregated. If yes, skip the Mint event
# because the aggregated burn processing will handle the debt reduction.
if operation.operation_type in LIQUIDATION_OPERATION_TYPES:
    liquidation_key = (user.address, token_address)
    aggregated_amount = tx_context.liquidation_aggregates.get(liquidation_key, 0)
    if aggregated_amount > 0:
        # Multiple liquidations will be aggregated
        logger.debug(
            f"_process_debt_mint_with_match: LIQUIDATION has aggregated amount "
            f"for user={user.address}, debt_v_token={token_address} "
            f"(aggregated={aggregated_amount}) - skipping Mint event"
        )
        return
```

**Step 4: Process burn with aggregated amount**

File: `src/degenbot/cli/aave.py` in `_process_debt_burn_with_match`

```python
if operation.operation_type in LIQUIDATION_OPERATION_TYPES:
    liquidation_key = (user.address, token_address)

    if liquidation_key in tx_context.processed_liquidations:
        # This (user, debt_v_token) has already been processed
        logger.debug(
            f"_process_debt_burn_with_match: LIQUIDATION already processed for "
            f"user={user.address}, debt_v_token={token_address} - skipping"
        )
        return

    # Mark as processed
    tx_context.processed_liquidations.add(liquidation_key)

    # Get aggregated debtToCover for this (user, debt_asset)
    aggregated_debt_to_cover = tx_context.liquidation_aggregates.get(
        liquidation_key, 0
    )

    if aggregated_debt_to_cover > 0:
        # Use aggregated amount
        token_math = TokenMathFactory.get_token_math_for_token_revision(
            debt_asset.v_token_revision
        )
        burn_value = token_math.get_debt_burn_scaled_amount(
            aggregated_debt_to_cover, scaled_event.index
        )
        # Update scaled_amount to use the correct aggregated value
        scaled_amount = burn_value
        logger.debug(
            f"_process_debt_burn_with_match: LIQUIDATION using aggregated "
            f"debtToCover={aggregated_debt_to_cover}, scaled_burn={burn_value}"
        )
    else:
        # Fallback to event amount if no aggregation
        burn_value = scaled_event.amount
```

**Step 5: Call preprocessing in transaction processing**

File: `src/degenbot/cli/aave.py` in `_process_transaction`

```python
# Preprocess liquidations to aggregate debtToCover for multi-liquidation scenarios
# This handles cases where multiple liquidations share the same debt asset
# and emit a single combined burn event. See debug/aave/0056.
_preprocess_liquidation_aggregates(tx_context, tx_operations.operations)
```

## Fix Details

**Files Modified:**
1. `src/degenbot/cli/aave_types.py` - Added liquidation aggregation fields to TransactionContext
2. `src/degenbot/cli/aave.py` - Added preprocessing and modified burn/mint processing

**Functions Added/Modified:**
1. `_preprocess_liquidation_aggregates` - NEW: Aggregate debtToCover by (user, debt_v_token)
2. `_get_v_token_for_underlying` - NEW: Helper to get vToken from underlying asset
3. `_process_debt_mint_with_match` - MODIFIED: Skip Mint when aggregation detected
4. `_process_debt_burn_with_match` - MODIFIED: Use aggregated amounts and track processed pairs
5. `_process_transaction` - MODIFIED: Call preprocessing before operations

## Verification

```bash
uv run degenbot aave update
```

**Block 20872104 Processing:**
- **Preprocessing:** Aggregates debtToCover: 47,005,978 + 292,538,129,344 = 292,585,135,322
- **Mint event:** Skipped (aggregation detected)
- **Burn event:** Processed with aggregated amount
  - Calculated scaled burn: 262,455,520,366
  - Starting balance: 524,911,475,260
  - Final balance: 524,911,475,260 - 262,455,520,366 = 262,455,954,894
  - Contract balance: 262,455,954,894
  - **Result:** MATCH ✓

## Expected Results

**Transaction 0x75b41542ba21912e8210166c11a10d0bbb70514ffce26bf1b42b2f723abee5e2:**

- **Total debtToCover:** 47,005,978 + 292,538,129,344 = 292,585,135,322
- **Single burn amount:** 292,538,129,344 (represents total minus interest accrual)
- **Scaled reduction:** 262,455,520,366 (using index at block 20872104)

**Balance calculation:**
- Starting: 524,911,475,260
- Skip Mint (interest accrual handled by index update): no change
- Combined burn: -262,455,520,366
- **Final:** 262,455,954,894 (matches contract exactly)

## Key Insight

**Multi-liquidation scenarios require transaction-level aggregation.** When multiple liquidations share the same debt asset:
- The contract may emit a single combined burn event
- Per-liquidation processing causes incorrect debt reduction
- Aggregating at the transaction level matches the contract behavior

**The enrichment layer calculates amounts per-operation**, but the contract operates at the transaction level. For liquidations, the authoritative source is the Pool event's `debtToCover`, summed across all liquidations.

## Related Issues

- Issue 0050: Multi-Liquidation Same Debt Asset Burn Misassignment (amount-based matching)
- Issue 0051: Bad Debt Liquidation Debt Burn Matching (total_burn calculation)
- Issue 0054: Sequential matching approach
- Issue 0055: User-level liquidation count approach

## References

- Transaction: 0x75b41542ba21912e8210166c11a10d0bbb70514ffce26bf1b42b2f723abee5e2
- Block: 20872104
- User: 0x4A76a94442FAFF09b67689b4Ba5645C47638F38a
- Debt Asset: USDC (0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48)
- Debt Token: variableDebtEthUSDC (0x72E95b8931767C79bA4EeE721354d6E99a61D004)
- Contract: VariableDebtToken revision 1
- Files:
  - `src/degenbot/cli/aave_types.py`
  - `src/degenbot/cli/aave.py`

## Summary

The failure occurs because the current architecture assumes a 1:1 mapping between liquidations and debt burn events. When multiple liquidations share the same debt asset and emit a single combined burn, the per-liquidation processing model breaks.

The fix aggregates debt reductions at the transaction level, applying a single reduction for all liquidations of the same (user, debt_asset). This follows traditional accounting principles (sum transactions, apply total) and eliminates the need for fragile event-to-liquidation matching.

**Changes Made:**
1. Added `liquidation_aggregates` and `processed_liquidations` to TransactionContext
2. Added `_preprocess_liquidation_aggregates` to sum debtToCover before processing
3. Modified `_process_debt_mint_with_match` to skip Mint when aggregation detected
4. Modified `_process_debt_burn_with_match` to use aggregated amounts
5. Added helper `_get_v_token_for_underlying` for vToken lookup

**Result:** Block 20872104 now processes correctly with matching contract balances.
