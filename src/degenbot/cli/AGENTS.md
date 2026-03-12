# Degenbot CLI - Agent Knowledge Base

This document captures lessons learned, architectural insights, and debugging guidance for the Degenbot CLI Aave processing system.

## Aave V3 Processing

### Interest Accrual - Critical Understanding

**Lesson:** Interest accrual Mint events are emitted for tracking purposes ONLY and do NOT increase the scaled balance.

**Context:** When processing Aave V3 transactions, the aToken contract emits Mint events during interest accrual (e.g., before transfers, withdrawals). However, these events are purely informational - they do not actually mint tokens or change the user's scaled balance.

**Contract Behavior (aToken rev_1.sol:2825-2855):**
```solidity
function _transfer(address sender, address recipient, uint256 amount, uint256 index) internal {
    // Calculate interest accrued
    uint256 senderBalanceIncrease = senderScaledBalance.rayMul(index) -
        senderScaledBalance.rayMul(_userState[sender].additionalData);
    
    // Update the stored index ONLY
    _userState[sender].additionalData = index.toUint128();
    
    // Transfer scaled balance
    super._transfer(sender, recipient, amount.rayDiv(index).toUint128());
    
    // Emit Mint event for TRACKING ONLY - no _mint() call!
    if (senderBalanceIncrease > 0) {
        emit Mint(_msgSender(), sender, senderBalanceIncrease, senderBalanceIncrease, index);
    }
}
```

**Key Insight:**
- Interest = `scaledBalance * (newIndex - oldIndex) / RAY`
- The user's **scaled balance** does not change
- The user's **effective balance** increases because the index increased
- Mint event `amount` field = interest in underlying units (for tracking)

**Implementation:**
```python
# In enrichment.py - INTEREST_ACCRUAL operations
if operation.operation_type.name == "INTEREST_ACCRUAL":
    raw_amount = scaled_event.amount  # Interest in underlying units
    scaled_amount = 0  # NO balance change - event is tracking-only
```

**Issue Reference:** `debug/aave/0004 - Interest Accrual Scaling Error in Enrichment.md`

---

### Pool Versions and Scaling

**Lesson:** Pool versions 1-8 pass unscaled amounts to the logic library; the logic library handles scaling.

**Context:** Different Aave V3 pool revisions handle amount scaling differently:

- **Pool revisions 1-8:** The Pool contract passes unscaled amounts (underlying units) to the token contract, which then calculates scaled amounts internally using `rayDiv(amount, index)`.
- **Pool revision 9+:** The Pool contract pre-calculates scaled amounts before calling the token contract.

**Python Implementation:**
The enrichment layer handles both cases uniformly:
1. Extract raw amount from Pool event (unscaled)
2. Calculate scaled amount using TokenMath:
   - Versions 1-3: `TokenMathV1` - standard `ray_div` (half-up rounding)
   - Version 4: `TokenMathV4` - floor/ceil rounding
   - Versions 5+: `TokenMathV5` - same as V4

**TokenMathFactory mapping:**
```python
_TOKEN_MATH = {
    1: TokenMathV1, 2: TokenMathV1, 3: TokenMathV1,
    4: TokenMathV4,
    5: TokenMathV5, 6: TokenMathV5, 7: TokenMathV5, 8: TokenMathV5, 9: TokenMathV5, 10: TokenMathV5,
}
```

---

### Debugging Failed Updates

**When an Aave update fails with balance verification errors:**

1. **Identify the failing transaction:**
   ```bash
   uv run degenbot aave update --chunk 1 2>&1 | tee debug_output.log
   ```

2. **Gather on-chain data:**
   - Get transaction receipt with all events
   - Get implementation address at the block (for proxy contracts)
   - Get scaled balances before and after the transaction
   - Call `ATOKEN_REVISION()` or `POOL_REVISION()` to determine contract version

3. **Read the contract source:**
   - Check `contract_reference/aave/` for the appropriate revision
   - Follow the execution path from Pool → Logic → Token
   - Pay special attention to how amounts are handled

4. **Verify assumptions:**
   - Are events emitted for actual state changes or just for tracking?
   - Are amounts in underlying units or scaled units?
   - Does the event update storage or just emit?

5. **Calculate expected vs actual:**
   - Starting balance + operations = expected final balance
   - Compare with on-chain balance at block + 1
   - Identify the discrepancy

**Common pitfalls:**
- Assuming events always represent state changes
- Confusing underlying amounts with scaled amounts
- Not accounting for version-specific rounding behavior
- Treating interest accrual as balance increases

---

### Architecture Overview

**Processing Pipeline:**

1. **Event Fetching** (`_build_transaction_contexts`)
   - Fetches logs from RPC
   - Categorizes events (Pool events, ScaledToken events, ERC20 transfers)
   - Groups by transaction

2. **Operation Parsing** (`aave_transaction_operations.py`)
   - Parses events into logical operations
   - Matches scaled token events to pool events
   - Determines operation types (SUPPLY, WITHDRAW, BORROW, REPAY, etc.)

3. **Enrichment** (`aave/enrichment.py`)
   - Calculates scaled amounts using TokenMath
   - Handles version-specific rounding
   - Creates validated event objects

4. **Processing** (`_process_operation`)
   - Routes to appropriate handlers
   - Updates user positions
   - Handles edge cases (interest accrual, balance transfers, etc.)

5. **Verification** (`_verify_scaled_token_positions`)
   - Compares calculated balances with on-chain balances
   - Raises assertion on mismatch

---

### Key Files and Their Roles

- `src/degenbot/cli/aave.py` - Main CLI and processing logic
- `src/degenbot/cli/aave_transaction_operations.py` - Event parsing and operation classification
- `src/degenbot/cli/aave_event_matching.py` - Event matching within operations
- `src/degenbot/aave/enrichment.py` - Amount calculation and validation
- `src/degenbot/aave/calculator.py` - TokenMath wrapper
- `src/degenbot/aave/libraries/token_math.py` - Version-specific math implementations
- `src/degenbot/aave/processors/` - Revision-specific token processors
- `contract_reference/aave/` - Contract source code for different revisions

---

### Testing Commands

```bash
# Run specific block range
uv run degenbot aave update --chunk 1

# Check specific transaction
uv run python3 -c "
from web3 import Web3
w3 = Web3(Web3.HTTPProvider('http://node:8545'))
tx = w3.eth.get_transaction_receipt('0x...')
print(tx)
"

# Verify on-chain balance
cast call <aToken_address> "scaledBalanceOf(address)" <user_address> --block <block_number> --rpc-url http://node:8545
```

---

### Contract References

**Location:** `contract_reference/aave/`

**Structure:**
```
Pool/
  rev_1.sol, rev_2.sol, ... rev_10.sol
AToken/
  rev_1.sol, rev_2.sol, ... rev_5.sol
VariableDebtToken/
  rev_1.sol, rev_3.sol, ... rev_5.sol
GhoVariableDebtToken/
  rev_1.sol, rev_2.sol, ... rev_6.sol
```

**Usage:** When debugging, always check the specific revision that was active at the time of the transaction. Revisions are stored in the database and can be retrieved via:
- `ATOKEN_REVISION()` for AToken contracts
- `POOL_REVISION()` for Pool contracts
- `DEBT_TOKEN_REVISION()` for VariableDebtToken contracts

---

### ERC20 Transfer vs BalanceTransfer Events

**Lesson:** ERC20 Transfer events and BalanceTransfer events have different properties and must be handled separately.

**Context:** Aave V3 emits two types of transfer events:
1. **BalanceTransfer** (AToken/vToken events with index) - Emitted by AToken contracts during liquidations, includes `amount` and `index`
2. **ERC20 Transfer** (standard ERC20 events) - Emitted for all token transfers, no index field

**The Problem:** Both events were categorized as `COLLATERAL_TRANSFER`, causing enrichment to fail when ERC20 Transfers (which have `index=None`) were processed in operations with pool events.

**Solution:** Created separate event types:
- `COLLATERAL_TRANSFER` / `DEBT_TRANSFER` - For BalanceTransfer events (have index)
- `ERC20_COLLATERAL_TRANSFER` / `ERC20_DEBT_TRANSFER` - For standard ERC20 Transfers (no index)

**Event Type Mapping:**
```python
# In aave_transaction_operations.py
class ScaledTokenEventType(StrEnum):
    COLLATERAL_TRANSFER = auto()      # AToken BalanceTransfer (has index)
    ERC20_COLLATERAL_TRANSFER = auto()  # Standard ERC20 Transfer (no index)
    DEBT_TRANSFER = auto()            # vToken BalanceTransfer (has index)
    ERC20_DEBT_TRANSFER = auto()      # Standard ERC20 Transfer (no index)
```

**Enrichment Handling:**
```python
# In enrichment.py - ERC20 transfers don't need index-based calculation
if scaled_event.event_type.value == "erc20_collateral_transfer":
    raw_amount = scaled_event.amount
    scaled_amount = scaled_event.amount  # 1:1 mapping
else:
    # Index-based calculation for BalanceTransfer events
    scaled_amount = calculator.calculate(...)
```

**Key Differences:**
- **BalanceTransfer**: Has index, amount is in scaled units, emitted by AToken contract
- **ERC20 Transfer**: No index, amount is in underlying units, emitted by ERC20 standard

**When to use each:**
- Use BalanceTransfer for actual balance movements with interest accrual
- Use ERC20 Transfer for simple token movements (e.g., liquidator receiving collateral)

---

### General Lessons

1. **Read the source code** - Don't assume behavior based on event names alone
2. **Follow the execution path** - Pool → Logic → Token, trace through all contracts
3. **Check storage updates** - Events may be emitted without actual state changes
4. **Version matters** - Different revisions have different behaviors
5. **Units are critical** - Always distinguish between underlying and scaled amounts
6. **On-chain is truth** - When in doubt, query the actual contract state
7. **Event names are misleading** - Same event type name can represent different underlying events

---

*Last updated: 2026-03-12*
*Contributors: Issue #0004 investigation team*
