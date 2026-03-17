# Issue 0020: Index Verification Failure Due to Out-of-Order Event Processing

## Date
2026-03-17

## Symptom
```
AssertionError: Index verification failure for AaveV3Asset(...). 
User AaveV3User(...) last_index (1001331979729743704723436158) does not match 
contract last_index (1001331991252538377398964857) at block 19695735
```

## Root Cause
Operations are processed in a specific order (0, 1, 2, 3, 4...) which does not always match the log index order of events within a transaction. When an earlier operation (lower log index) is processed after a later operation (higher log index), it overwrites the `last_index` with a lower value, causing the verification to fail.

### Transaction Analysis
In the failing transaction at block 19695735:
- **Operation 3** (INTEREST_ACCRUAL at logIndex=45): index = `1001331979729743704723436158`
- **Operation 2** (WITHDRAW at logIndex=66): index = `1001331991252538377398964857`

When Operation 3 was processed after Operation 2, it overwrote the higher index with the lower index, causing a mismatch with the on-chain state.

## Technical Details

### The Problem
The Aave V3 contract updates the user's `last_index` (stored liquidity index) whenever they interact with their position. This index should reflect the **most recent** liquidity index at the time of their last interaction. 

When we process operations out of log index order:
1. WITHDRAW at logIndex=66 sets `last_index` to `1001331991252538377398964857`
2. INTEREST_ACCRUAL at logIndex=45 (processed later in order) sets `last_index` to `1001331979729743704723436158`
3. Result: Our calculated `last_index` is lower than the contract's value

### Affected Functions
All functions that update `last_index` were unconditionally setting the value:

1. `_process_scaled_token_operation` - Core function for all token events
2. `_process_collateral_mint_with_match` - Collateral mint processing
3. `_process_collateral_burn_with_match` - Collateral burn processing  
4. `_process_debt_mint_with_match` - Debt mint processing (GHO and non-GHO)
5. `_process_debt_burn_with_match` - Debt burn processing

## Fix

Applied "only update if greater" logic to all `last_index` updates:

```python
# Before (unconditional update):
position.last_index = new_index

# After (conditional update):
if new_index > (position.last_index or 0):
    position.last_index = new_index
```

### Files Modified
- `src/degenbot/cli/aave.py`

### Specific Changes

1. **`_process_scaled_token_operation`** (lines 2038, 2057, 2070, 2083)
   - All collateral and debt mint/burn cases now check if new_index > current

2. **`_process_collateral_mint_with_match`** (line ~2799-2800)
   - Added current_index variable and comparison

3. **`_process_collateral_burn_with_match`** (line ~2995-2998)
   - Added conditional check before setting last_index

4. **`_process_debt_mint_with_match`** 
   - GHO section: Added conditional check (line ~3131)
   - Non-GHO section: Added conditional check (line ~3227)

5. **`_process_debt_burn_with_match`**
   - GHO section: Added conditional check (line ~3322)
   - Bad debt liquidation case: Added conditional check (line ~3376)
   - Normal liquidation case: Added conditional check (line ~3429)

## Verification

After applying the fix:
```bash
$ uv run degenbot aave update --chunk 1
AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True) 
successfully updated to block 19,695,735
```

The transaction at block 19695735 now processes without errors, and the index verification passes.

## Key Insight

**Operations are not always processed in log index order.** When building operations from events, the classification and enrichment logic may create operations in a different sequence than the events appear in the transaction. The `last_index` must always reflect the **maximum** index from any operation affecting that position, not just the most recently processed operation.

This matches the contract behavior where `last_index` is set to the current liquidity index at the time of each user interaction, and since the liquidity index only increases over time (within a block), the stored value should always be the highest index encountered.

## Related Issues

This fix is complementary to existing fixes for similar ordering issues:
- Issue #0005: Token Revision vs Pool Revision Mismatch
- Issue #0006: BalanceTransfer Event Double Assignment
- Issue #0013: WITHDRAW Interest Accrual Mint Matching

## Test Coverage

The fix was verified with:
- Single block update at the failing block (19695735)
- Multi-block updates (5 blocks, 10 blocks)
- Various transaction types including SUPPLY, BORROW, WITHDRAW, REPAY

All tests pass with correct balance and index verification.
