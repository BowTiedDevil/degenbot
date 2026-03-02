# Issue 0018: stkAAVE Transfer Double Counting in Pending Delta Calculation

**Date:** 2026-03-02

**Symptom:**
```
AssertionError: User 0x2079C29Be9c8095042edB95f293B5b510203d6cE: GHO discount 0 does not match GHO vDebtToken contract (3000) @ 0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B at block 17968296
```

**Root Cause:**
The `_get_or_init_stk_aave_balance` function calculates a "pending delta" for stkAAVE transfers that occur after the current event (higher log index). This is designed to handle reentrancy where the GHO debt token contract sees the post-transfer balance before the Transfer event is emitted.

However, stkAAVE transfers are processed BEFORE GHO operations (in `_process_transaction`, lines 2294-2311). When a transfer is processed:
1. The transfer handler updates `user.stk_aave_balance` immediately
2. Later, when a GHO mint/burn handler runs, it calls `_get_or_init_stk_aave_balance`
3. That function adds the pending delta from transfers with higher log indices
4. BUT the transfer was already applied in step 1!
5. This causes double-counting, resulting in incorrect balances

In the failing case:
- User receives 38,709 stkAAVE in block 17968292
- User redeems 138,709 stkAAVE in block 17968296  
- GHO interest accrues at block 17968296
- The pending delta calculation incorrectly includes the already-processed redeem transfer
- Results in negative balance: -61,291 tokens
- This causes discount to be calculated as 0 instead of 3000 (30%)

**Transaction Details:**
- **Hash:** 0xde650c9761da03899d38d4db0b6ac64bd1376186869589f3605e361963a07329 (block 17968292)
- **Hash:** 0x9e06894bea16229ca6f5859f2f395ff5d89e465a3c9e7c871493a3f5fd74b36c (block 17968296)
- **Type:** stkAAVE transfer and redeem
- **User:** 0x2079C29Be9c8095042edB95f293B5b510203d6cE (luggis.eth)
- **Asset:** stkAAVE, GHO

**Fix:**
Added tracking for processed stkAAVE transfers to prevent double-counting:

1. Added `processed_stk_aave_transfers: set[int]` field to `TransactionContext` (line 138)
2. In `_process_stk_aave_transfer_event`, mark transfers as processed after updating balances (line 1148)
3. In `get_pending_stk_aave_delta_at_log_index`, skip transfers that have already been processed (line 230)

**Files Modified:**
- `src/degenbot/cli/aave.py`

**Key Insight:**
When processing events in a specific order (stkAAVE transfers before GHO operations), the pending delta mechanism needs to be aware of which transfers have already been applied to avoid double-counting.

**Refactoring:**
The current approach of processing stkAAVE transfers before GHO operations and then using pending delta calculations is complex and error-prone. A cleaner design would be to:
1. Process ALL events in strict log index order
2. Update stkAAVE balances immediately when Transfer events are encountered
3. When GHO mint/burn events are encountered, use the current stkAAVE balance directly
4. Remove the pending delta mechanism entirely

This would simplify the code and eliminate the possibility of double-counting.
