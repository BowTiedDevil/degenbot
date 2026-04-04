# Degenbot CLI - Agent Knowledge Base

This document captures lessons learned, architectural insights, and debugging guidance for the Degenbot CLI Aave processing system.

## Aave V3 Processing

### Interest Accrual - Critical Understanding

**Lesson:** Interest accrual Mint events are emitted for tracking purposes ONLY and do NOT increase the scaled balance.

**Context:** When processing Aave V3 transactions, the aToken contract emits Mint events during interest accrual (e.g., before transfers, withdrawals). However, these events are purely informational - they do not actually mint tokens or change the user's scaled balance.

**Contract Behavior (aToken rev_1.sol:2825-2855):**
```solidity
function _transfer(address sender, address recipient, uint256 amount, uint256 index) internal {
    // Calculate interest accrued (tracks interest earned since last interaction)
    uint256 senderBalanceIncrease = senderScaledBalance.rayMul(index) -
        senderScaledBalance.rayMul(_userState[sender].additionalData);
    
    // Update the stored index ONLY - this is the ONLY state change for interest accrual
    _userState[sender].additionalData = index.toUint128();
    
    // Transfer scaled balance
    super._transfer(sender, recipient, amount.rayDiv(index).toUint128());
    
    // Emit Mint event for TRACKING ONLY - no actual _mint() is called!
    // This event is emitted solely for off-chain tracking purposes.
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

**Lesson:** Pool revisions and token revisions are independent versioning systems that affect different aspects of amount handling.

**Context:** Two separate versioned systems exist:

1. **Pool Revisions** (affects how amounts are passed between contracts):
   - **Pool revisions 1-8:** The Pool contract passes unscaled amounts (underlying units) to the token contract
   - **Pool revision 9+:** The Pool contract pre-calculates scaled amounts before calling the token contract

2. **Token Revisions** (affects rounding math behavior):
   - **Token revisions 1-3:** Uses `HalfUpRoundingMath` - standard `ray_div` with half-up rounding
   - **Token revision 4+:** Uses `ExplicitRoundingMath` - explicit floor/ceil rounding

**Python Implementation:**
The enrichment layer handles both systems independently:
1. Pool revision determines if raw amounts need scaling before processing
2. Token revision determines which rounding math to use via TokenMathFactory

**TokenMathFactory mapping (by pool version):**
```python
_TOKEN_MATH = {
    1: HalfUpRoundingMath,
    2: HalfUpRoundingMath,
    3: HalfUpRoundingMath,
    4: ExplicitRoundingMath,
    5: ExplicitRoundingMath,
    6: ExplicitRoundingMath,
    7: ExplicitRoundingMath,
    8: ExplicitRoundingMath,
    9: ExplicitRoundingMath,
    10: ExplicitRoundingMath,
}
```

**Key Point:** While the mapping uses pool version numbers for lookup, the actual rounding behavior is determined by the token revision at runtime. Pool and token revisions typically move together but are technically independent.

---

### Math Library Architecture

**Lesson:** Three levels of math abstraction separate concerns between Pool, Token, and GHO operations.

**Architecture:**

The Aave module uses a layered math architecture:

1. **`wad_ray_math.py`** - Low-level primitives
   - `ray_mul`, `ray_div` - Half-up rounding
   - `ray_mul_floor`, `ray_mul_ceil` - Explicit rounding
   - `wad_mul`, `wad_div` - Wad arithmetic

2. **TokenMath classes** - Token-level calculations
   - `HalfUpRoundingMath` - Pool revs 1-3 (standard half-up)
   - `ExplicitRoundingMath` - Pool revs 4+ (floor/ceil rounding)
   - Factory pattern via `TokenMathFactory.get_token_math(pool_version)`

3. **PoolMath** - Pool-level calculations
   - `get_treasury_mint_amount()` - MINT_TO_TREASURY operations
   - `underlying_to_scaled_collateral()` - Reverse calculations
   - `underlying_to_scaled_debt()` - Reverse calculations

4. **GhoMath** - GHO-specific calculations
   - `calculate_discount_rate()` - stkAAVE discount rates
   - `calculate_discounted_balance()` - Discounted debt amount
   - `calculate_effective_debt_balance()` - Post-discount balance

**Key Principle:** Pool and Token revisions are independent:
- Use **PoolMath** for Pool-level operations (MINT_TO_TREASURY)
- Use **TokenMath** for Token-level operations (mint/burn/transfer)
- Use **GhoMath** for GHO discount calculations

**Files:**
- `src/degenbot/aave/libraries/wad_ray_math.py` - Primitives
- `src/degenbot/aave/libraries/token_math.py` - Token calculations
- `src/degenbot/aave/libraries/pool_math.py` - Pool calculations
- `src/degenbot/aave/libraries/gho_math.py` - GHO calculations

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
# Run Python tests
just test-python

# Run specific block range
uv run degenbot aave update --chunk 1

# Check specific transaction
cast receipt 0x... --rpc-url http://node:8545

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

### Execution Flow Diagrams

**Location:** `docs/cli/aave.md`

When debugging complex interactions or unexpected event sequences, reference the Mermaid flow diagrams in this file which show:
- Complete execution paths from Pool public functions through logic libraries
- Event emission points and their triggers
- Decision points and conditional logic
- Library call chains and dependencies

**Key diagrams for debugging:**
- **Supply/Withdraw Flow** - Shows interest accrual handling and collateral validation
- **Borrow/Repay Flow** - Shows isolation mode logic and debt token operations
- **Liquidation Flow** - Complete liquidation path with collateral/debt handling
- **Flash Loan Flow** - Shows callback pattern and debt vs repayment paths
- **Library Dependencies** - Visual map of which libraries call which

These diagrams help identify:
- Which library emits a particular event
- What validations run before state changes
- Where amounts are scaled/unscaled
- How interest accrual flows through the system

**Tip:** If an event is missing or unexpected, trace backwards from the event in the diagram to find the conditions that trigger it.

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
    COLLATERAL_TRANSFER = auto()  # AToken BalanceTransfer (has index)
    ERC20_COLLATERAL_TRANSFER = auto()  # Standard ERC20 Transfer (no index)
    DEBT_TRANSFER = auto()  # vToken BalanceTransfer (has index)
    ERC20_DEBT_TRANSFER = auto()  # Standard ERC20 Transfer (no index)
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

### ERC20 Transfer and BalanceTransfer Event Matching

**Lesson:** When pairing ERC20 Transfer events with BalanceTransfer events during operation creation, the event types must be explicitly matched as compatible pairs.

**Context:** In `_create_transfer_operations`, the code attempts to pair ERC20 Transfer events with their corresponding BalanceTransfer events (which provide index data for proper scaling). The original matching logic required exact type equality (`bt_ev.event_type == ev.event_type`), but since ERC20 Transfers and BalanceTransfers have different types, valid pairs were never matched.

**The Problem:** Without proper matching:
1. The BalanceTransfer event wouldn't be marked as `local_assigned`
2. The standalone BalanceTransfer processing loop would create a duplicate operation
3. Validation would fail with "Event at logIndex X assigned to multiple operations"

**Example Transaction:** `0x4a88a8c6a43b5df2ee59ebcf266225fbc5b876f202009422f0f9d05cc4915f35`
- logIndex 104: `ERC20_COLLATERAL_TRANSFER` (ERC20 Transfer)
- logIndex 107: `COLLATERAL_TRANSFER` (BalanceTransfer with index)
- These represent the same transfer but have different event types

**Solution:** Allow matching between compatible event type pairs:

```python
# In _create_transfer_operations method
event_types_match = (
    bt_ev.event_type == ev.event_type
    or {bt_ev.event_type, ev.event_type}
    == {
        ScaledTokenEventType.COLLATERAL_TRANSFER,
        ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
    }
    or {bt_ev.event_type, ev.event_type}
    == {
        ScaledTokenEventType.DEBT_TRANSFER,
        ScaledTokenEventType.ERC20_DEBT_TRANSFER,
    }
)
```

**Key Insight:**
- Set comparison (`{a, b} == {c, d}`) elegantly checks for bidirectional matching
- Both directions are covered: ERC20 Transfer looking for BalanceTransfer AND BalanceTransfer looking for ERC20 Transfer
- This fix applies to both the unassigned events search AND the existing operations search

---

### Semantic Matching for Event Association

**Lesson:** Associate events with operations based on **semantic relationships** (user, asset, operation type) rather than numeric comparisons or positional proximity.

**Context:** When processing complex transactions like multi-asset liquidations, events may be emitted in unpredictable order and contain amounts in incompatible units. Amount-based matching fails because:
- `debtToCover` is in underlying units while burn amounts are in scaled units
- Batch transactions emit events out of log index order
- Different revisions handle unit conversions differently

**The Problem:** The original matching logic compared burn amounts to `debtToCover`:

```python
# Amount-based matching (FRAGILE)
total_burn = ev.amount + (ev.balance_increase or 0)
if total_burn < debt_to_cover - TOLERANCE:
    continue  # Skip this event - amounts don't match!
```

This failed because:
- 20,647,496 (scaled wstETH) vs 8,414,088,469,488,124,792 (underlying GHO)
- Different assets, different units, no valid comparison possible

**Solution:** Use semantic matching based on immutable identifiers:

```python
# Semantic matching (ROBUST)
if ev.user_address == user and event_token_address == debt_v_token_address:
    # Match! The burn belongs to this liquidation regardless of amounts
    matched_burns.append(ev)
```

**Core Principle:**
> If a debt burn exists for the same user and asset in the same transaction, it belongs to the liquidation.

This works because:
1. Each user has at most one debt position per asset
2. Liquidations burn ALL debt positions for the user
3. No other operation burns debt in the same transaction

**When to Use Semantic Matching:**
- Events have clear ownership (user + asset)
- Transaction context establishes causality  
- Amount comparisons are unreliable (mixed units, revisions)
- Events may be out of order (batch transactions)

**Validation Strategy:**
Separate matching from validation:
- **Matching**: Find events by semantic criteria (user + asset)
- **Processing**: Validate business logic (amounts, balances)

```python
# Matching phase - trust the semantic relationship
matched_event = find_event(user=user, asset=asset)

# Processing phase - validate amounts make sense
if matched_event.amount > position.balance * 2:
    logger.warning(f"Unusually large burn: {matched_event.amount}")
```

**Benefits:**
- Simpler code without unit conversions
- More robust to event ordering issues
- Easier to understand and debug
- Future-proof across contract revisions

**Trade-offs:**
- Less validation at match time (caught during processing instead)
- Trusts contract behavior (assumes semantic matches are correct)
- Requires clear documentation of assumptions

**Documentation:**
See full architectural documentation: `docs/architecture/semantic-matching.md`

**Issue Reference:** `debug/aave/0029 - Multi-Asset Liquidation Missing Secondary Debt Burns Fix.md`

---

### General Lessons

1. **Read the source code** - Don't assume behavior based on event names alone
2. **Follow the execution path** - Pool → Logic → Token, trace through all contracts
3. **Check storage updates** - Events may be emitted without actual state changes
4. **Version matters** - Different revisions have different behaviors
5. **Units are critical** - Always distinguish between underlying and scaled amounts
6. **On-chain is truth** - When in doubt, query the actual contract state
7. **Event names are misleading** - Same event type name can represent different underlying events

### Burn Event Matching

**Lesson:** When matching Burn events to WITHDRAW operations, the total burned is `amount + balanceIncrease`, not `amount - balanceIncrease`.

**Context:** The Aave V3 Burn event has the following structure:
```solidity
event Burn(address indexed from, address indexed target, uint256 value, uint256 balanceIncrease, uint256 index);
```

- `value`: Principal amount being burned (in underlying units)
- `balanceIncrease`: Accrued interest since last user interaction
- **Total burned** = `value` + `balanceIncrease`

This total matches the Withdraw event's amount exactly. A single-character error (`-` instead of `+`) caused burn events to fail matching, leaving them unassigned and classified as INTEREST_ACCRUAL operations (which zero out the scaled amount).

**Issue Reference:** `debug/aave/0007 - Interest Accrual Burn Amount Zeroed in Enrichment.md`

---

*Last updated: 2026-03-18*
*Contributors: Issue #0004, #0007, #0029 investigation teams*
