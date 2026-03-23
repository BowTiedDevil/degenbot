# Issue 0054: Multi-Liquidation Similar Amounts Range Overlap

## Date
2026-03-23

## Symptom
```
TransactionValidationError: Operation 0 (LIQUIDATION) validation failed:
Multiple debt burns for same asset in LIQUIDATION. Debt burns: [882, 901]. Token addresses: ['0x1b7D3F4b3c032a5AE656e30eeA4e8E1Ba376068F']
```

## Root Cause

The transaction contains **two liquidations for the same user with the same debt asset (CRV)** but different collateral assets (DAI and USDC). The `debtToCover` values are nearly identical:
- First liquidation: 10,938,700,354,773,724,930 CRV
- Second liquidation: 11,033,524,126,091,068,318 CRV

The range-based matching (50%-150% tolerance) created overlapping ranges:
- First range: [5.47e18, 1.64e19]
- Second range: [5.52e18, 1.66e19]

**Both burns fell within BOTH ranges**, causing the first liquidation to collect both burns while the second got none.

## Transaction Details

| Field | Value |
|-------|-------|
| **Hash** | 0x18fa073dae64a28a3782ef63f9d3a2c09a534030966459771deb71c28be65cb4 |
| **Block** | 22977988 |
| **Type** | Multi-Asset Liquidation (Same Debt, Different Collateral) |
| **User** | 0xcf8cf5dF28dB4F4e8376C90D8CEbd5f7A4F73620 |
| **Liquidator** | 0xd4bC53434C5e12cb41381A556c3c47e1a86e80E3 |
| **Debt Asset** | CRV (0xD533a949740bb3306d119CC777fa900bA034cd52) |
| **vCRW** | 0x1b7D3F4b3c032a5AE656e30eeA4e8E1Ba376068F |
| **Pool Revision** | 8 |

### Event Ordering

| Event | LogIndex | Description |
|-------|----------|-------------|
| vCRW Burn | 882 | Debt burn for Liquidation #1 |
| aDAI Burn | 886 | Collateral burn for Liquidation #1 |
| LiquidationCall | 893 | Pool event for Liquidation #1 |
| vCRW Burn | 901 | Debt burn for Liquidation #2 |
| aUSDC Burn | 905 | Collateral burn for Liquidation #2 |
| LiquidationCall | 912 | Pool event for Liquidation #2 |

## Fix

**Status:** ✅ IMPLEMENTED AND VERIFIED

**File:** `src/degenbot/cli/aave_transaction_operations.py`

**Function:** `_collect_debt_burns`

**Location:** Lines 1994-2069

### Why Sequential Matching Works

From the Aave V3 liquidation flow:

```
LiquidationLogic.executeLiquidationCall():
    1. _burnDebtTokens()      → Emits Burn event
    2. Collateral management  → Emits collateral events
    3. Transfer repayment
    4. emit LiquidationCall() → Emits LiquidationCall event
```

**Guaranteed ordering within each liquidation:**
1. Debt burn event is emitted FIRST
2. LiquidationCall event is emitted LAST

**For multiple liquidations:**
```
Liquidation 1: Burn (882) → ... → LiquidationCall (893)
Liquidation 2: Burn (901) → ... → LiquidationCall (912)
```

This is guaranteed by blockchain execution semantics - events are emitted in the order code executes.

### Implementation

Replaced range-based matching with sequential matching by logIndex:

1. Collect all candidate burns for the user/debt_asset, sorted by logIndex
2. Track liquidation position among all liquidations for this (user, debt_asset)
3. For multi-liquidation scenarios: assign burn[i] to liquidation[i]

**Key code change in `_collect_debt_burns`:**

```python
# Collect all candidate burns for this user/debt_asset, sorted by logIndex
candidate_burns = sorted(
    [
        ev
        for ev in scaled_events
        if ev.event["logIndex"] not in assigned_indices
        and ev.user_address == user
        and ev.event_type in {ScaledTokenEventType.DEBT_BURN, ScaledTokenEventType.GHO_DEBT_BURN}
        and get_checksum_address(ev.event["address"]) == debt_v_token_address
    ],
    key=lambda e: e.event["logIndex"],
)

if is_multi_liquidation and len(candidate_burns) > 1:
    # Sequential matching: take the burn at our position
    if liquidation_position < len(candidate_burns):
        burns.append(candidate_burns[liquidation_position])
        assigned_indices.add(candidate_burns[liquidation_position].event["logIndex"])
```

### Advantages Over Range-Based Matching

1. **Relies on guaranteed execution order** - Blockchain semantics ensure burn[i] → liquidation[i]
2. **No amount comparisons** - Immune to rounding/interest edge cases
3. **Simpler logic** - O(n log n) sorting vs O(n*m) comparisons
4. **Self-documenting** - Code reflects actual contract execution flow

## Key Insight

**Match burns to liquidations by execution order, not by amount.**

The contract emits events in a deterministic sequence:
1. Each `liquidationCall()` executes completely before the next
2. Within each call: burn events → LiquidationCall event
3. Events are emitted in execution order (logIndex = emission order)

Therefore: **The Nth burn belongs to the Nth liquidation.**

This is more robust than amount-based matching because:
- No edge cases with similar debtToCover values
- No sensitivity to rounding or interest accrual
- Relies on fundamental blockchain property, not calculation heuristics

## Related Issues

- Issue 0053: Multi-Liquidation Same Asset GHO Liquidation Validation Error (introduced 50%-150% range)
- Issue 0050: Multi-Liquidation Same Debt Asset Burn Misassignment (introduced >10x skip logic)
- Issue 0052: GHO Liquidation Skips Secondary Debt Burns (unified burn collection)

## Verification

**Test command:**
```bash
uv run degenbot aave update
```

**Result:** ✅ PASSED

Block 22977988 processed without validation errors. Both liquidation operations had exactly 1 debt burn each:
- Operation for logIndex 893: 1 debt burn at logIndex 882
- Operation for logIndex 912: 1 debt burn at logIndex 901

## References

- Transaction: 0x18fa073dae64a28a3782ef63f9d3a2c09a534030966459771deb71c28be65cb4
- Block: 22977988
- Investigation report: /tmp/aave_0054_tx_report.md
- Contract flow: docs/aave/flows/liquidation.md
- Contract source: contract_reference/aave/Pool/rev_10.sol
