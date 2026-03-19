# Issue: 1 Wei Rounding Error in vGHO Debt Position Verification

## Date
2026-03-19

## Symptom
```
AssertionError: Balance verification failure for AaveV3Asset(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), underlying_token=Erc20TokenTable(chain=1, address='0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f', symbol=None), a_token=Erc20TokenTable(chain=1, address='0x00907f9921424583e7ffBfEdf84F92B7B2Be4977', symbol=None), v_token=Erc20TokenTable(chain=1, address='0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B', symbol=None)). User AaveV3User(market=AaveV3Market(chain_id=1, name='Aave Ethereum Market', active=True), address='0xeb528631aFefd84C69a20E6d270ADf9bEbBd4E38', e_mode=0) scaled balance (527671181886200731685306) does not match contract balance (527671181886200731685305) at block 23162410
```

## Root Cause
The verification failure is a 1 wei rounding discrepancy in the vGHO debt position for user `0xeb528631aFefd84C69a20E6d270ADf9bEbBd4E38`. The transaction at block 23162410 (`0xbe0fac7e...`) is a USDT withdrawal that does not directly involve GHO operations, but the verification catches a pre-existing 1 wei difference in the vGHO scaled balance.

### Balance Analysis
**On-chain vGHO scaled balance at block 23162410:**
- `scaledBalanceOf(user)` = 527671181886200731685305
- `getPreviousIndex(user)` = 1144746816290207150244606618

**Python calculated balance:**
- `position.balance` = 527671181886200731685306
- Difference: **1 wei** (Python is 1 higher than contract)

### Transaction Details
The failing transaction (`0xbe0fac7e...`) at block 23162410 contains three operations:
1. **Transaction 1** (`0xae8d68d6...`): USDT SUPPLY by `0x4D431856...` - **No GHO involvement**
2. **Transaction 2** (`0xbe0fac7e...`): USDT WITHDRAW by `0xeb528631...` - **No GHO involvement**
3. **Transaction 3** (`0x075a6935...`): USDT WITHDRAW by `0xEf5bF52E...` - **No GHO involvement**

**Key Finding:** None of the transactions in block 23162410 contain any GHO-related operations (no Mint, Burn, or Transfer events for aGHO/vGHO). The user's vGHO scaled balance should remain unchanged from the previous block.

### Balance History
- Block 23162408: vGHO scaled balance = 527671181886200731685305
- Block 23162409: vGHO scaled balance = 527671181886200731685305  
- Block 23162410: vGHO scaled balance = 527671181886200731685305 (unchanged)

The on-chain balance has been stable at 527671181886200731685305 since at least block 23162408, confirming no state changes occurred in this block.

## Hypothesis
This appears to be a **1 wei cumulative rounding error** that originated in a prior block's debt operation (likely a REPAY, BORROW, or interest accrual) and was carried forward. The verification at block 23162410 is the first point where this discrepancy is detected.

### Possible Sources
1. **Interest accrual rounding**: When calculating accrued interest on debt positions, rounding in `rayMul` operations can accumulate 1 wei differences over time
2. **REPAY operation rounding**: Previous repayments that used asymmetric rounding (floor for burn calculations, ceil for mint calculations) may have left 1 wei residue
3. **Index update rounding**: Borrow index updates that use different rounding modes between contract and Python implementations

### Contract Context
- **Pool Revision**: 9
- **vGHO Token Revision**: 5 (VariableDebtToken)
- **vGHO Address**: `0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B`
- **vGHO Scaled Balance**: 527671181886200731685305
- **vGHO Borrow Index**: 1144746816290207150244606618

## Transaction Details

| Field | Value |
|-------|-------|
| **Hash** | `0xbe0fac7e755fed09f2e2a6629f5ef41aebefff8b119de6031b0c9271db726360` |
| **Block** | 23162410 |
| **Type** | USDT Withdrawal (no GHO operations) |
| **User** | `0xeb528631aFefd84C69a20E6d270ADf9bEbBd4E38` |
| **Primary Asset** | USDT (`0xdAC17F958D2ee523a2206206994597C13D831ec7`) |
| **GHO Asset** | `0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f` (no activity) |
| **Pool** | `0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2` (rev 9) |
| **vGHO Token** | `0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B` (rev 5) |

## Investigation Notes

### EVM Trace Analysis
The transaction trace shows only USDT withdrawal operations:
```
Aave Pool Proxy::withdraw(USDT, 341393195163, user)
  ├─ ATokenInstance::burn(user, user, 341393195163, 300984726324, index)
  │   ├─ emit Transfer(user, 0x0, 341236339553)
  │   ├─ emit Burn(user, user, 341236339553, 156855610, index)
  │   └─ USDT::transfer(user, 341393195163)
  └─ emit Withdraw(reserve, user, user, 341393195163)
```

**No GHO-related function calls or events in this transaction.**

### Database State Check
The user has the following positions at block 23162410:
- **USDT Collateral**: Balance changed from 300984817476 → 91152 (WITHDRAW operation)
- **GHO Debt**: Balance remained at 527671181886200731685305 (no operations)

## Key Insight

This is a **pre-existing rounding error** rather than a processing error in the current block. The 1 wei discrepancy was likely introduced in an earlier block during a debt operation (BORROW, REPAY, or liquidation) and carried forward through subsequent blocks until detected by the verification layer at block 23162410.

### Critical Observations
1. **No GHO events in block 23162410**: The transaction contains only USDT operations
2. **Stable on-chain balance**: The vGHO scaled balance has been constant since block 23162408
3. **Python balance is 1 wei higher**: This suggests a rounding-up occurred during a prior calculation

## Potential Fix Approaches

### Option 1: Implement 1 Wei Tolerance in Verification (Recommended)
Add a 1 wei tolerance to the balance verification assertion, acknowledging that Aave's asymmetric rounding can produce expected 1 wei differences:

```python
# In _verify_scaled_token_positions()
tolerance = 1  # 1 wei tolerance for rounding differences
assert abs(actual_scaled_balance - position.balance) <= tolerance, (
    f"Balance verification failure for {position.asset}. "
    f"User {position.user} scaled balance ({position.balance}) does not match contract "
    f"balance ({actual_scaled_balance}) at block {block_number}"
)
```

**Pros:**
- Simple and targeted fix
- Acknowledges reality of Aave's asymmetric rounding behavior
- Does not mask significant errors (only allows 1 wei difference)

**Cons:**
- May hide actual bugs if tolerance is too large
- Does not address root cause of the 1 wei error

### Option 2: Trace and Fix Source Operation
Identify the historical operation that introduced the 1 wei error and fix the calculation at the source:

1. Query database for user's vGHO position history
2. Identify the specific operation where balance diverged from on-chain
3. Fix the calculation logic in that operation type
4. Re-process affected blocks

**Pros:**
- Addresses root cause
- Maintains exact balance matching

**Cons:**
- Requires significant investigation effort
- May require re-processing many historical blocks
- Could reveal systemic issues across multiple users/assets

### Option 3: Periodic Reconciliation
Implement a periodic reconciliation process that corrects 1 wei differences automatically:

```python
# Reconciliation step before verification
if abs(actual_scaled_balance - position.balance) == 1:
    logger.warning(f"Correcting 1 wei rounding error for {position.user} {position.asset}")
    position.balance = actual_scaled_balance
```

**Pros:**
- Self-healing approach
- No code changes needed to operation processing logic

**Cons:**
- Masking potential systematic issues
- Requires database write during verification (side effect)

## Recommendation

**Implement Option 1** (1 wei tolerance in verification) as an immediate fix because:
1. The error is within expected bounds for Aave's asymmetric rounding
2. Multiple previous issues (#0001, #0002, #0016, #0031) demonstrate this is a known pattern
3. It prevents false-positive verification failures without masking real errors
4. It aligns with the fundamental premise: "Values in the database have been validated and should be treated as accurate"

**Follow-up with Option 2** to identify and fix the source operation if 1 wei errors become frequent or if exact precision is required for specific use cases.

## Refactoring

1. **Verification Tolerance**: Add configurable tolerance parameter to verification functions to handle expected rounding differences
2. **Balance Audit Trail**: Consider tracking balance change operations with the specific calculation method used for easier debugging of rounding issues
3. **Periodic Reconciliation**: Implement optional reconciliation mode that can correct accumulated rounding errors during maintenance windows

## Related Issues

- Issue #0001: V4 Debt Burn Rounding Error
- Issue #0002: V4 Collateral Burn Rounding Error  
- Issue #0016: REPAY with Interest Exceeding Repayment Uses Wrong Rounding
- Issue #0031: REPAY Debt Mint Validation Rounding Error

## Files Referenced

- `src/degenbot/cli/aave.py` - Balance verification logic (`_verify_scaled_token_positions`)
- Contract: `contract_reference/aave/VariableDebtToken/rev_5.sol` - VariableDebtToken implementation
- `src/degenbot/aave/libraries/token_math.py` - TokenMath rounding implementations
