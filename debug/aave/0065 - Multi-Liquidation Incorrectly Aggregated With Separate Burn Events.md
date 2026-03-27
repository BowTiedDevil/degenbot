# 0065 - Multi-Liquidation Incorrectly Aggregated With Separate Burn Events

**Issue:** Debt balance verification failure for user with multiple liquidations

**Date:** 2026-03-26

## Symptom

```
AssertionError: Debt balance verification failure for AaveV3Asset(..., symbol='USDC').
User 0x36cc7B13029B5DEe4034745FB4F24034f3F2ffc6 
scaled balance (1066907346952) does not match contract balance (1066670574246) 
at block 24366566

Difference: 236,772,706 (236.77 USDC)
```

## Transaction Details

- **Hash:** `0x267bfb06abdfb44516196382d28f2578797320bb7547ab00d928f115570fcb26`
- **Block:** 24366566
- **Market:** Aave Ethereum Market (Pool revision 10)
- **User:** `0x36cc7B13029B5DEe4034745FB4F24034f3F2ffc6`
- **Debt Asset:** USDC (variableDebtEthUSDC)
- **Collateral Asset:** BAL (aEthBAL)

### Transaction Structure

This transaction contains **6 separate liquidations** of the same user, all using:
- Same debt asset: USDC
- Same collateral asset: BAL
- Different amounts of debtToCover

### Events Breakdown

| LogIndex | Event Type | Amount | debtToCover | Notes |
|----------|------------|--------|-------------|-------|
| 2 | Burn (USDC debt) | 1,089,233,668 | 1,089,705,668 | Liquidation 1 |
| 14 | LiquidationCall | - | 1,089,705,668 | BAL/USDC |
| 32 | Burn (USDC debt) | 7,845,723,135 | 7,845,723,136 | Liquidation 2 |
| 42 | LiquidationCall | - | 7,845,723,136 | BAL/USDC |
| 61 | Burn (USDC debt) | 196,238,425 | 196,238,425 | Liquidation 3 |
| 71 | LiquidationCall | - | 196,238,425 | BAL/USDC |
| 104 | Burn (USDC debt) | 214,387,955 | 214,387,956 | Liquidation 4 |
| 114 | LiquidationCall | - | 214,387,956 | BAL/USDC |
| 146 | Burn (USDC debt) | 53,361,495 | 53,361,495 | Liquidation 5 |
| 156 | LiquidationCall | - | 53,361,495 | BAL/USDC |
| 172 | Burn (USDC debt) | 20,230,790 | 20,230,791 | Liquidation 6 |
| 182 | LiquidationCall | - | 20,230,791 | BAL/USDC |

**Key Finding:** Each liquidation has its **own separate Burn event** with its own amount.

## Root Cause

The multi-liquidation logic assumed that when multiple liquidations share the same (user, debt_asset), there would be a **single combined burn event** representing the total debt reduction (COMBINED_BURN pattern from Issue 0056).

However, in this transaction, 6 liquidations each emit their own separate Burn event (SEPARATE_BURNS pattern). The parser's burn collection logic was checking `liquidation_count == len(candidate_burns)`, but `candidate_burns` shrinks as burns get assigned, causing later liquidations to fail the check and fall into the COMBINED_BURN branch which only assigns burns to position 0.

### Why This Happens

In `_collect_debt_burns` (aave_transaction_operations.py):
1. Operation 0 (position 0): Gets burn[0], marks it assigned
2. Operation 1 (position 1): Now `candidate_burns` has 5 burns, check is `6 == 5` = False
3. Falls into COMBINED_BURN branch which only assigns to position 0
4. Operation 1 gets no burns, falls into INTEREST_ACCRUAL instead

## The Problem Flow

1. **Parsing:** Transaction parser creates 6 LIQUIDATION operations
2. **Burn Collection:** `_collect_debt_burns` tries to assign burns to each liquidation
3. **Pattern Detection Fails:** Due to shrinking candidate list, only first 3 liquidations get burns
4. **Remaining Burns:** Last 3 burns end up in INTEREST_ACCRUAL operations
5. **Processing:** Interest accrual operations zero out the scaled amount (tracking-only events)
6. **Result:** Only 3/6 debt burns are actually applied to the position

## Key Insight

> **Multi-liquidation scenarios can have either:**
> 1. **COMBINED_BURN:** N liquidations share M burns where M < N (Issue 0056)
> 2. **SEPARATE_BURNS:** N liquidations have N burns, one per liquidation (this issue)
>
> The processing logic must distinguish between these patterns and handle each correctly.

## Fix

### Files Modified

1. **Created:** `src/degenbot/aave/liquidation_patterns.py` - Pattern detection module
2. **Modified:** `src/degenbot/cli/aave_types.py` - Added `LiquidationPatternContext`
3. **Modified:** `src/degenbot/cli/aave.py` - Pattern-aware processing logic
4. **Modified:** `src/degenbot/cli/aave_transaction_operations.py` - Fixed burn assignment

### Key Changes

**1. Pattern Detection (liquidation_patterns.py)**
```python
class LiquidationPattern(Enum):
    SINGLE = auto()          # 1 liquidation, 1 burn
    COMBINED_BURN = auto()   # N liquidations, < N burns
    SEPARATE_BURNS = auto()  # N liquidations, N burns

def detect_liquidation_patterns(operations, scaled_events):
    # Groups liquidations by (user, debt_v_token)
    # Counts liquidations vs burns
    # Returns pattern for each group
```

**2. Transaction Context (aave_types.py)**
```python
@dataclass
class TransactionContext:
    liquidation_patterns: LiquidationPatternContext
    scaled_token_events: list  # Added for pattern detection
    # Removed: liquidation_aggregates, liquidation_counts, processed_liquidations
```

**3. Burn Collection (aave_transaction_operations.py)**
```python
def _collect_debt_burns(...):
    # Get ALL burns for this (user, debt_asset) BEFORE any assignments
    all_burns_for_asset = [...]
    total_burn_count = len(all_burns_for_asset)
    
    if liquidation_count_for_asset == total_burn_count:
        # SEPARATE_BURNS: Each liquidation gets burn at its position
        target_burn = all_burns_for_asset[liquidation_position]
    elif liquidation_count_for_asset > total_burn_count:
        # COMBINED_BURN: All burns go to first liquidation
        if liquidation_position == 0:
            assign_all_burns()
```

**4. Processing Logic (aave.py)**
```python
def _process_debt_burn_with_match(...):
    pattern = tx_context.liquidation_patterns.get_pattern(user, token)
    
    if pattern == LiquidationPattern.SINGLE:
        # Use operation's individual debt_to_cover
    elif pattern == LiquidationPattern.COMBINED_BURN:
        # Use aggregated amount, process once
    elif pattern == LiquidationPattern.SEPARATE_BURNS:
        # Use individual amount, process each burn
```

## Verification

After fix, block 24366566 processes correctly:

**Operations parsed:**
- Operation 0: LIQUIDATION with DEBT_BURN (logIndex=78)
- Operation 1: LIQUIDATION with DEBT_BURN (logIndex=108)
- Operation 2: LIQUIDATION with DEBT_BURN (logIndex=137)
- Operation 3: LIQUIDATION with DEBT_BURN (logIndex=180)
- Operation 4: LIQUIDATION with DEBT_BURN (logIndex=222)
- Operation 5: LIQUIDATION with DEBT_BURN (logIndex=248)

**All 6 burns now correctly assigned to their respective liquidations**

**Debt reductions applied:**
- Burn 1: 895,938,414 scaled
- Burn 2: 6,450,626,950 scaled
- Burn 3: 161,344,066 scaled
- Burn 4: 176,266,317 scaled
- Burn 5: 43,872,960 scaled
- Burn 6: 16,633,429 scaled
- **Total: 7,744,682,136 scaled** ✓

**Balance verification:** PASSED
```
AaveV3Market successfully updated to block 24,366,566
```

## Related Issues

- **0056:** Multi-Liquidation Single Debt Burn Misassignment - COMBINED_BURN pattern
- **0060:** Liquidation Burn Event Processed Before LiquidationCall Event

## Architectural Improvements

1. **Centralized Pattern Detection:** Pattern detection now happens once in preprocessing rather than ad-hoc in processing
2. **Clear Abstraction:** `LiquidationPattern` enum makes code self-documenting
3. **Separation of Concerns:** Parser handles burn assignment, processor handles amount calculation
4. **Type Safety:** Typed dataclasses instead of raw dict lookups

## Test Coverage

Add test cases for both multi-liquidation patterns:
- Multiple liquidations, 1 burn event (Issue 0056 - COMBINED_BURN)
- Multiple liquidations, N burn events (Issue 0065 - SEPARATE_BURNS)

---

**Status:** ✅ FIXED

**Verification:** Block 24366566 processes successfully with correct debt balance
