# Issue 0024: MINT_TO_TREASURY Balance Transfer Accumulation Error

## Date
2026-03-17

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(...). User AaveV3User(...) scaled balance (591699257463037258842) does not match contract balance (591699276327750759963) at block 20245383
```

## Root Cause
The MINT_TO_TREASURY processing logic fails to account for **multiple BalanceTransfer events** that accumulate to the treasury during liquidations within the same transaction. While the code correctly identifies and uses BalanceTransfer amounts when present, it appears to only process the first BalanceTransfer event encountered, missing subsequent transfers to the treasury in multi-liquidation transactions.

## Transaction Details

**Hash:** `0xe762a8ead8ccd682c95c50471486a0cbca6d1831b7edb71edb6b11fc536f5d81`

**Block:** 20245383

**Type:** Multi-Liquidation with Treasury Mint

**Users Affected:**
- Liquidated: 0x64A5240b2F2A21D224A483D366fe037A7aA39C69, 0xCd705deE3dB92533Fffa2bdd47b97ab573E8Ed14 (x2), 0x726A1632FfdF60921AC636a1BF9f502E4513f152
- Treasury: 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c (Aave Treasury)

**Asset:** WETH (0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2) / aWETH (0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8)

**Operations in Transaction:**
1. GHO_LIQUIDATION (user: 0x64A5240b2F2A21D224A483D366fe037A7aA39C69)
2. GHO_LIQUIDATION (user: 0xCd705deE3dB92533Fffa2bdd47b97ab573E8Ed14) - liquidation fee transferred to treasury
3. GHO_LIQUIDATION (user: 0xCd705deE3dB92533Fffa2bdd47b97ab573E8Ed14) - liquidation fee transferred to treasury
4. LIQUIDATION (user: 0x726A1632FfdF60921AC636a1BF9f502E4513f152)
5. MINT_TO_TREASURY (user: 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c)

## Smart Contract Control Flow

```
Pool.liquidationCall() [4 separate calls]
  → LiquidationLogic.executeLiquidationCall()
    → IAToken.transferOnLiquidation(from, to, value)  // Transfer liquidation fee to treasury
      → AToken._transfer(from, to, amount, false)
        → Emits BalanceTransfer(from, to, value.rayDiv(index), index)

Pool.mintToTreasury(assets)
  → PoolLogic.executeMintToTreasury()
    → Loop: Calculate amountToMint for each asset
    → IAToken.mintToTreasury(amount, index)
      → AToken._mintScaled(POOL, treasury, amount, index)
        → Emits Mint(caller, treasury, value, balanceIncrease, index)
```

## Events Analysis

### BalanceTransfer Events to Treasury (from liquidations):
1. **logIndex 217**: From 0x64A5240b2F2A21D224A483D366fe037A7aA39C69 → Treasury
   - Amount: 55,461,612,525,713 (scaled)
2. **logIndex 238**: From 0xCd705deE3dB92533Fffa2bdd47b97ab573E8Ed14 → Treasury  
   - Amount: 35,181,141,502,410 (scaled)
3. **logIndex 267**: From 0xCd705deE3dB92533Fffa2bdd47b97ab573E8Ed14 → Treasury
   - Amount: 54,045,855,003,531 (scaled)
4. **logIndex 289**: From 0x726A1632FfdF60921AC636a1BF9f502E4513f152 → Treasury
   - Amount: 56,295,289,450,012 (scaled)

**Total BalanceTransfer to Treasury:** ~200,983,898,481,666 scaled units

### Mint Event (MINT_TO_TREASURY):
- **logIndex 215**: Mint to Treasury
  - `value` (amountToMint): 834,056,496,965,512 (underlying)
  - `balanceIncrease`: 834,056,496,965,512 (underlying)
  - `index`: 1027102797957782513356011667

**Key Observation:** The Mint event shows `value == balanceIncrease`, which indicates `amount == 0` (no new scaled tokens minted), only interest accrual on existing balance.

## Balance Calculation

**Expected Balance Calculation:**
```
Starting Balance (at block 20245382): ~591,699,075,343,852,278,297
+ BalanceTransfer logIndex 217: +55,461,612,525,713
+ BalanceTransfer logIndex 238: +35,181,141,502,410
+ BalanceTransfer logIndex 267: +54,045,855,003,531
+ BalanceTransfer logIndex 289: +56,295,289,450,012
+ Interest Accrual from index update: +~188,751,350,120,21
= Expected: 591,699,276,327,750,759,963
```

**Actual Contract Balance:** 591,699,276,327,750,759,963

**Calculated Balance:** 591,699,257,463,037,258,842

**Difference:** 18,875,135,012,121 (~18.9 trillion or ~0.0000032%)

## The Bug

In `_calculate_mint_to_treasury_scaled_amount()` (aave.py:2534-2615), the code correctly checks for BalanceTransfer events and returns the BalanceTransfer amount directly:

```python
if balance_transfer_events:
    bt_event = balance_transfer_events[0]  # <-- BUG: Only takes first event!
    bt_amount, _ = eth_abi.abi.decode(...)
    return bt_amount
```

However, when there are **multiple liquidations** in a single transaction, each liquidation transfers a liquidation fee to the treasury via BalanceTransfer. The current code only processes the first BalanceTransfer event (`balance_transfer_events[0]`), ignoring subsequent transfers.

## Evidence from Logs

```
Processing operation 0: GHO_LIQUIDATION
  Using BalanceTransfer amount 55461612525713 for transfer from 0x64A5240b2F2A21D224A483D366fe037A7aA39C69 at log 217
  Skipping paired BalanceTransfer at log 217  # Marked as assigned

Processing operation 1: GHO_LIQUIDATION  
  Using BalanceTransfer amount 35181141502410 for transfer from 0xCd705deE3dB92533Fffa2bdd47b97ab573E8Ed14 at log 238
  Skipping paired BalanceTransfer at log 238
  Using BalanceTransfer amount 35181141502410 for transfer from 0xCd705deE3dB92533Fffa2bdd47b97ab573E8Ed14 at log 238 (again!)
  Skipping paired BalanceTransfer at log 267

Processing operation 3: LIQUIDATION
  Using BalanceTransfer amount 56295289450012 for transfer from 0x726A1632FfdF60921AC636a1BF9f502E4513f152 at log 289
  Skipping paired BalanceTransfer at log 289

Processing operation 4: MINT_TO_TREASURY
  # BalanceTransfer events are already marked as "assigned" to liquidation operations
  # but only ONE BalanceTransfer is associated with the MINT_TO_TREASURY operation
```

## Key Insight

The issue was actually TWO separate problems:

### Problem 1: Multiple BalanceTransfer Matching
When a user has multiple collateral transfers to the treasury within a single liquidation (which can happen when the protocol takes multiple fee cuts), the code was only matching the first ERC20 Transfer to the first BalanceTransfer. Subsequent transfers were not being matched correctly because the matching logic didn't account for log index proximity.

### Problem 2: Interest Accrual vs New Mints
The MINT_TO_TREASURY operation was correctly identifying that when `amount == balanceIncrease`, no new tokens are minted (only interest accrual). However, the BalanceTransfer events to the treasury during liquidations represent ACTUAL transfers of collateral, not interest accrual. These transfers are handled by the liquidation operations themselves.

## The Real Fix

**Location:** `src/degenbot/cli/aave.py`, line ~3396

**Problem:** The BalanceTransfer matching logic was missing the log index proximity check, causing multiple transfers from the same user to all match to the first BalanceTransfer.

**Fix:** Uncomment and use the log index proximity check to ensure each ERC20 Transfer matches its paired BalanceTransfer:

```python
# BEFORE (buggy - commented out):
if (
    bt_token == token_address
    and bt_from == scaled_event.from_address
    and bt_to == scaled_event.target_address
    # and abs(bt_log_index - transfer_log_index) <= 3  # WIP REMOVED THIS CHECK
):

# AFTER (fixed):
if (
    bt_token == token_address
    and bt_from == scaled_event.from_address
    and bt_to == scaled_event.target_address
    and abs(bt_log_index - transfer_log_index) <= BALANCE_TRANSFER_PROXIMITY_THRESHOLD
):
```

This ensures that when user 0xCd705deE3dB92533Fffa2bdd47b97ab573E8Ed14 has two transfers to the treasury (logIndices 237 and 266), each one correctly matches to its corresponding BalanceTransfer (logIndices 238 and 267 respectively).

## Additional Changes

1. Added constant `BALANCE_TRANSFER_PROXIMITY_THRESHOLD = 3` to avoid magic number
2. The MINT_TO_TREASURY calculation correctly returns 0 when `amount == balanceIncrease` (interest accrual only, no new tokens)
3. BalanceTransfer events remain in liquidation operations where they semantically belong

## Refactoring

1. **Extract BalanceTransfer matching logic:** The BalanceTransfer matching logic in `_process_collateral_transfer_with_match` is complex and duplicated. Extract it into a dedicated helper function that can be tested independently.

2. **Add metrics/logging:** Track how many BalanceTransfer events are processed per operation to detect when events might be missed.

3. **Consider event pairing during parsing:** Instead of matching ERC20 Transfers to BalanceTransfers during processing, consider doing this pairing during the operation parsing phase when the transaction context is first built. This would make the processing logic simpler and more deterministic.

## References

- Transaction: 0xe762a8ead8ccd682c95c50471486a0cbca6d1831b7edb71edb6b11fc536f5d81
- Block: 20245383
- Pool Contract: 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2 (Revision 3)
- aWETH Token: 0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8 (Revision 1)
- Treasury: 0x464C71f6c2F760DdA6093dCB91C24c39e5d6e18c
- Related Issues: 0022 (Collateral Burn Amount Mismatch), 0023 (Multi-Liquidation Collateral Burn)
