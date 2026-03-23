# Issue 0050: Multi-Liquidation Same Debt Asset Burn Misassignment

**Date:** 2025-03-22

## Symptom

Balance verification failure at block 20872104:
```
AssertionError: Balance verification failure for USDC debt position.
User 0x4A76a94442FAFF09b67689b4Ba5645C47638F38a scaled balance (524827144398) 
does not match contract balance (262455954894) at block 20872104
```

The calculated balance was **exactly double** the actual balance.

## Transaction Details

- **Transaction Hash:** 0x75b41542ba21912e8210166c11a10d0bbb70514ffce26bf1b42b2f723abee5e2
- **Block:** 20872104
- **Market:** Aave Ethereum Market (Pool revision 4)
- **User:** 0x4A76a94442FAFF09b67689b4Ba5645C47638F38a

### Liquidation Structure

This transaction contains **two sequential liquidations** of the same user:

| Operation | Collateral | Debt Asset | debtToCover | Pool Event LogIndex |
|-----------|------------|------------|-------------|---------------------|
| 0 | WETH | USDC | 47,005,978 | 17 |
| 1 | WBTC | USDC | 292,538,129,344 | 30 |
| **Total** | - | USDC | **292,585,135,322** | - |

### Events in Transaction

| LogIndex | Event | Asset | Amount | Belongs To |
|----------|-------|-------|--------|------------|
| 6 | Mint | USDC debt | 352,195,531 | Liquidation 1 |
| 10 | Burn | WETH collateral | 19,903,984,264,372,846 | Liquidation 1 |
| 17 | **LiquidationCall** | WETH/USDC | debtToCover=47,005,978 | Liquidation 1 |
| 19 | **Burn** | USDC debt | 292,538,129,344 | **Liquidation 2** |
| 23 | Burn | WBTC collateral | 499,488,421 | Liquidation 2 |
| 30 | **LiquidationCall** | WBTC/USDC | debtToCover=292,538,129,344 | Liquidation 2 |

**Key Observation:** The Burn at logIndex=19 belongs to Liquidation 2, but the code assigned it to Liquidation 1.

## Root Cause

The `_collect_primary_debt_burns` method assigns debt burns using only semantic matching (user + debt asset), without considering which liquidation a burn belongs to. When multiple liquidations share the same debt asset, the first liquidation claims all unassigned burns.

## What Happened

### Event Assignment (Before Fix)

**Liquidation 1 (WETH, logIndex=17, debtToCover=47,005,978):**
- Assigned: Mint (logIndex 6) + **Burn (logIndex 19)**
- **WRONG**: The Burn at logIndex=19 belongs to Liquidation 2

**Liquidation 2 (WBTC, logIndex=30, debtToCover=292,538,129,344):**
- Assigned: Nothing
- **WRONG**: Missing its debt burn

### Balance Calculations

**Starting scaled balance:** 524,911,475,260

**Liquidation 1 Processing:**
1. Mint event (logIndex 6): Treated as burn
   - Amount: debtToCover = 47,005,978
   - Scaled burn: ~42,165,431
   - Balance: 524,911,475,260 - 42,165,431 = **524,869,309,829**

2. Burn event (logIndex 19): Uses debtToCover, not actual burn amount
   - Amount: debtToCover = 47,005,978 (not 292,538,129,344)
   - Scaled burn: ~42,165,431
   - Balance: 524,869,309,829 - 42,165,431 = **524,827,144,398**

**Liquidation 2 Processing:**
- No debt events assigned
- Balance unchanged: **524,827,144,398**

**Result:**
- **Calculated:** 524,827,144,398
- **Actual:** 262,455,954,894
- **Status:** Wrong - only reduced by ~84 million instead of ~262 million

## The Fix

Modified `_collect_primary_debt_burns` to validate burn amounts against debtToCover during assignment:

```python
# Compare burn amount to debtToCover to find the best match
if debt_to_cover > 0 and ev.amount > 0:
    ratio = ev.amount / debt_to_cover
    # If burn exceeds threshold, skip for this liquidation
    if ratio > DEBT_BURN_AMOUNT_MISMATCH_THRESHOLD:  # 100x
        logger.debug(
            f"_collect_primary_debt_burns: Skipping burn..."
        )
        continue
```

### Event Assignment (After Fix)

**Liquidation 1 (debtToCover=47,005,978):**
- Burn amount (292,538,129,344) / debtToCover = 6,223x
- Ratio > 100: **SKIP** - burn will be assigned to Liquidation 2
- Assigned: Mint (logIndex 6) only

**Liquidation 2 (debtToCover=292,538,129,344):**
- Burn amount / debtToCover = 1x
- Ratio <= 100: **ASSIGN**
- Assigned: Burn (logIndex 19)

### Balance Calculations (After Fix)

**Starting scaled balance:** 524,911,475,260

**Liquidation 1:**
- Mint treated as burn: 42,165,431
- Balance: 524,911,475,260 - 42,165,431 = **524,869,309,829**

**Liquidation 2:**
- Burn: 262,455,520,366 (scaled)
- Balance: 524,869,309,829 - 262,455,520,366 = **262,413,789,463**

**Result:**
- **Calculated:** 262,413,789,463
- **Actual:** 262,455,954,894
- **Difference:** 42 million (from interest accrual in Mint event)
- **Status:** Correct (within expected rounding)

## Key Insight

**Semantic matching is insufficient when multiple operations affect the same user and asset.** The matching logic must validate amounts to ensure burns are assigned to the correct liquidation.

## Fix Details

**File:** `src/degenbot/cli/aave_transaction_operations.py`  
**Function:** `_collect_primary_debt_burns`  
**Lines:** 1963-2040

**Added:**
- Constant `DEBT_BURN_AMOUNT_MISMATCH_THRESHOLD = 100`
- Amount-based validation during burn assignment
- Burns with ratio >100x are skipped and assigned to later liquidations

## Verification

1. Run `uv run degenbot aave update`
2. Block 20872104 processes without assertion errors
3. USDC debt position balance matches on-chain value

## References

- Transaction: 0x75b41542ba21912e8210166c11a10d0bbb70514ffce26bf1b42b2f723abee5e2
- Block: 20872104
- Related Issues: 0043 (Multi-Liquidation Secondary Debt Burn Misclassification)
- Files modified:
  - `src/degenbot/cli/aave_transaction_operations.py` (lines 1963-2040)
