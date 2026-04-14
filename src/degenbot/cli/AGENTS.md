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

2. **Token Revisions** (affects rounding math behavior and processor selection):
   - **Token revisions 1-3:** Uses `HalfUpRoundingMath` - standard `ray_div` with half-up rounding
   - **Token revision 4+:** Uses `ExplicitRoundingMath` - explicit floor/ceil rounding

**Python Implementation:**
The enrichment layer handles both systems independently:
1. Pool revision determines if raw amounts need scaling before processing (for pool rev 9+)
2. Token revision determines which processor and rounding math to use via factories

**Processor mapping (by token revision):**
```python
# TokenProcessorFactory.COLLATERAL_PROCESSORS
{
    1: CollateralV1Processor,  # HalfUpRoundingMath
    2: CollateralV1Processor,  # Same as rev 1
    3: CollateralV3Processor,  # HalfUpRoundingMath
    4: CollateralV4Processor,  # ExplicitRoundingMath
    5: CollateralV5Processor,  # ExplicitRoundingMath
}

# TokenProcessorFactory.DEBT_PROCESSORS
{
    1: DebtV1Processor,
    2: DebtV1Processor,  # Same as rev 1
    3: DebtV3Processor,
    4: DebtV4Processor,
    5: DebtV5Processor,
}

# TokenProcessorFactory.GHO_DEBT_PROCESSORS
{
    1: GhoV1Processor,  # No discount
    2: GhoV2Processor,  # Discount support
    3: GhoV2Processor,   # Same as rev 2
    4: GhoV4Processor,  # Discount deprecated
    5: GhoV5Processor,  # Explicit rounding
    6: GhoV5Processor,   # Same as rev 5
}
```

**TokenMath mapping (by pool version):**
```python
# TokenMathFactory._TOKEN_MATH
{
    1: HalfUpRoundingMath,   # Pool revs 1-3
    2: HalfUpRoundingMath,
    3: HalfUpRoundingMath,
    4: ExplicitRoundingMath, # Pool revs 4-10
    5: ExplicitRoundingMath,
    6: ExplicitRoundingMath,
    7: ExplicitRoundingMath,
    8: ExplicitRoundingMath,
    9: ExplicitRoundingMath,
    10: ExplicitRoundingMath,
}
# Use get_token_math_for_token_revision(revision) to map token revision → pool version
```

**Key Point:** Token revision determines both the processor and rounding math. Pool and token revisions typically move together but are technically independent. Use `TokenProcessorFactory` to get the correct processor and `TokenMathFactory` for math operations.

---

### Math Library Architecture

**Lesson:** Four levels of abstraction separate concerns: primitives, math libraries, processors, and enrichment.

**Architecture:**

The Aave module uses a layered architecture:

1. **`wad_ray_math.py`** - Low-level primitives
   - `ray_mul`, `ray_div` - Half-up rounding
   - `ray_mul_floor`, `ray_mul_ceil` - Explicit rounding
   - `wad_mul`, `wad_div` - Wad arithmetic

2. **TokenMath classes** (`token_math.py`)
   - `HalfUpRoundingMath` - Pool revs 1-3 (standard half-up)
   - `ExplicitRoundingMath` - Pool revs 4+ (floor/ceil rounding)
   - Factory pattern via `TokenMathFactory.get_token_math_for_token_revision(revision)`

3. **Processors** (`processors/`)
   - Stateless revision-specific processors for mint/burn events
   - `CollateralV1Processor`, `CollateralV3Processor`, etc. - aToken processors
   - `DebtV1Processor`, `DebtV3Processor`, etc. - Standard vToken processors
   - `GhoV1Processor`, `GhoV2Processor`, etc. - GHO processors with discount handling
   - Created via `TokenProcessorFactory`

4. **Enrichment** (`enrichment.py` + `calculator.py`)
   - `ScaledEventEnricher` - Main entry point for event enrichment
   - `ScaledAmountCalculator` - Uses TokenMath via calculator

**Key Principle:** Processors are stateless and return deltas:
```python
# Processor calculates delta without modifying state
result = processor.process_mint_event(
    event_data=CollateralMintEvent(...),
    previous_balance=user_balance,  # For reference only
    previous_index=user_index,      # For reference only
)
# Caller applies the result
user_balance += result.balance_delta
user_index = result.new_index
```

**Key Principle:** Pool and Token revisions are independent:
- Processors use **TokenMath** for Token-level rounding
- **PoolMath** handles Pool-level operations (MINT_TO_TREASURY)
- **GhoMath** handles GHO discount calculations (used by GHO processors)

**Files:**
- `src/degenbot/aave/libraries/wad_ray_math.py` - Primitives
- `src/degenbot/aave/libraries/token_math.py` - Token calculations
- `src/degenbot/aave/libraries/pool_math.py` - Pool calculations
- `src/degenbot/aave/libraries/gho_math.py` - GHO calculations
- `src/degenbot/aave/processors/` - Revision-specific processors

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

1. **Event Fetching** (`aave_event_filtering.py`)
   - Fetches logs from RPC
   - Categorizes events (Pool events, ScaledToken events, ERC20 transfers)
   - Groups by transaction

2. **Operation Parsing** (`aave_transaction_operations.py`)
   - Parses events into logical operations
   - Matches scaled token events to pool events
   - Determines operation types (SUPPLY, WITHDRAW, BORROW, REPAY, etc.)

3. **Enrichment** (`aave/enrichment.py`)
   - Calculates scaled amounts using ScaledAmountCalculator
   - Handles version-specific rounding via TokenMathFactory
   - Creates validated event objects

4. **Processing** (Processor classes in `aave/processors/`)
   - Revision-specific processors handle mint/burn calculations
   - Stateless processors return deltas; callers apply results
   - GHO processors handle discount calculations separately

5. **Verification** (Position verification in CLI)
   - Compares calculated balances with on-chain balances
   - Raises assertion on mismatch

---

### Key Files and Their Roles

**CLI Layer** (`src/degenbot/cli/`):
- `aave_transaction_operations.py` - Event parsing and operation classification
- `aave_event_filtering.py` - Event filtering and transaction context building
- `aave_types.py` - Type definitions for CLI operations
- `aave_utils.py` - Utility functions for CLI operations
- `database.py` - CLI database operations
- `exchange.py` - Exchange-related CLI commands
- `pool.py` - Pool-related CLI commands
- `utils.py` - General CLI utilities
- `aave/` - Aave-specific CLI subpackage
  - `commands.py` - Aave CLI command definitions
  - `constants.py` - Aave-specific constants
  - `db_assets.py`, `db_market.py`, `db_positions.py`, `db_users.py`, `db_verification.py` - Database models/queries
  - `event_fetchers.py`, `event_handlers.py` - Event fetching and handling
  - `extraction.py` - Aave amount extraction
  - `liquidation_processor.py` - Liquidation-specific processing
  - `token_processor.py` - Token-level processing
  - `transaction_processor.py` - Transaction-level processing
  - `transfers.py` - Transfer event handling
  - `verification.py` - Balance verification
  - `stkaave.py` - stkAAVE processing
  - `types.py`, `utils.py`, `erc20_utils.py` - Supporting types and utilities

**Aave Module** (`src/degenbot/aave/`):
- `enrichment.py` - Amount calculation and validation
- `extraction.py` - Raw amount extraction from pool events
- `calculator.py` - ScaledAmountCalculator using TokenMath
- `models.py` - Enriched event type definitions
- `events.py` - Event type enums and constants (ScaledTokenEventType, AaveV3PoolEvent, ERC20Event)
- `operation_types.py` - Operation type enum definitions
- `deployments.py` - Aave deployment configuration
- `liquidation_patterns.py` - Liquidation event pattern recognition
- `pattern_types.py` - Pattern type definitions
- `position_analysis.py` - Position analysis logic

**Math Libraries** (`src/degenbot/aave/libraries/`):
- `token_math.py` - TokenMath classes and TokenMathFactory
- `wad_ray_math.py` - Low-level ray math primitives
- `pool_math.py` - Pool-level calculations
- `gho_math.py` - GHO-specific discount calculations
- `percentage_math.py` - Percentage math operations

**Processors** (`src/degenbot/aave/processors/`):
- `base.py` - Protocol definitions and result dataclasses
- `factory.py` - TokenProcessorFactory for creating processors by revision
- `collateral/v1.py`, `v3.py`, `v4.py`, `v5.py` - aToken processors
- `debt/v1.py`, `v3.py`, `v4.py`, `v5.py` - Standard vToken processors
- `debt/gho/v1.py`, `v2.py`, `v4.py`, `v5.py` - GHO discount processors

**Contract References**:
- `contract_reference/aave/` - Contract source code for different revisions
  - `Pool/` - Pool contract revisions 1-10
  - `AToken/` - AToken contract revisions 1-5
  - `VariableDebtToken/` - VariableDebtToken revisions 1, 3-5
  - `GhoVariableDebtToken/` - GHO VariableDebtToken revisions 1-6
  - `AaveOracle/`, `GhoDiscountRateStrategy/`, `RewardsController/`, `stkAAVE/` - Supporting contracts

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

### Processor Architecture

**Lesson:** Revision-specific processors provide clean, stateless event processing that mirrors contract behavior.

**Context:** The refactoring moved from monolithic calculation logic in `calculator.py` to revision-specific processors. Each processor handles mint/burn events for a specific token revision, matching the on-chain contract behavior exactly.

**Processor Types:**

1. **CollateralTokenProcessor** (`processors/collateral/`)
   - Handles aToken (collateral) mint/burn events
   - Revisions 1-5 supported
   - V1: Half-up rounding, standard interest accrual
   - V3: Pre-calculation for interest-exceeds-withdrawal cases
   - V4+: Explicit floor/ceil rounding

2. **DebtTokenProcessor** (`processors/debt/`)
   - Handles standard vToken mint/burn events (non-GHO)
   - Revisions 1-5 supported
   - Similar progression as collateral processors

3. **GhoDebtTokenProcessor** (`processors/debt/gho/`)
   - Handles GHO variable debt tokens with discount mechanism
   - Revisions 1-6 supported
   - V1: No discount support
   - V2+: Full discount accrual on mint/burn
   - V4+: Discount deprecated, uses explicit rounding

**Processor Factory:**

```python
from degenbot.aave.processors.factory import TokenProcessorFactory

# Get processor for collateral (aToken) by revision
collateral_processor = TokenProcessorFactory.get_collateral_processor(revision=4)

# Get processor for standard debt (vToken)
debt_processor = TokenProcessorFactory.get_debt_processor(revision=4)

# Get processor for GHO debt
gho_processor = TokenProcessorFactory.get_gho_debt_processor(revision=5)
```

**Result Types:**

All processors return frozen dataclasses with the calculated delta:

```python
@dataclass(frozen=True, slots=True)
class ScaledTokenMintResult:
    balance_delta: int   # Change in scaled balance
    new_index: int       # New liquidity/debt index
    is_repay: bool       # True for repay/withdrawal

@dataclass(frozen=True, slots=True)
class ScaledTokenBurnResult:
    balance_delta: int   # Change in scaled balance (negative)
    new_index: int       # New liquidity/debt index

@dataclass(frozen=True, slots=True)
class GhoScaledTokenMintResult:
    balance_delta: int
    new_index: int
    user_operation: GhoUserOperation  # GHO_BORROW, GHO_REPAY, etc.
    discount_scaled: int              # Discount amount
    should_refresh_discount: bool     # Whether to refresh discount rate
```

**Key Benefits:**

1. **Exact contract matching** - Each processor revision mirrors the on-chain contract
2. **Stateless design** - Processors don't modify state, making them testable and composable
3. **Type safety** - Protocol definitions ensure consistent interfaces
4. **Clear separation** - GHO discount logic isolated from standard debt logic

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
# In aave/events.py (ScaledTokenEventType enum)
class ScaledTokenEventType(Enum):
    # Balance modifying event types
    COLLATERAL_BURN = auto()
    COLLATERAL_MINT = auto()
    COLLATERAL_TRANSFER = auto()  # AToken BalanceTransfer (has index)
    DEBT_BURN = auto()
    DEBT_MINT = auto()
    DEBT_TRANSFER = auto()  # vToken BalanceTransfer (has index)
    DISCOUNT_TRANSFER = auto()
    GHO_DEBT_BURN = auto()
    GHO_DEBT_MINT = auto()
    GHO_DEBT_TRANSFER = auto()
    # ERC20 transfer events
    ERC20_COLLATERAL_TRANSFER = auto()  # Standard ERC20 Transfer (no index)
    ERC20_DEBT_TRANSFER = auto()  # Standard ERC20 Transfer (no index)
    # Interest accrual event types (derived during enrichment)
    COLLATERAL_INTEREST_BURN = auto()
    COLLATERAL_INTEREST_MINT = auto()
    DEBT_INTEREST_BURN = auto()
    DEBT_INTEREST_MINT = auto()
    GHO_DEBT_INTEREST_BURN = auto()
    GHO_DEBT_INTEREST_MINT = auto()
```

**Enrichment Handling:**
```python
# In enrichment.py - ERC20 transfers don't need index-based calculation
elif scaled_event.event_type == ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER:
    raw_amount = scaled_event.amount
    scaled_amount = scaled_event.amount  # 1:1 mapping
else:
    # Index-based calculation for BalanceTransfer events
    scaled_amount = calculator.calculate(...)
```

**Key Differences:**
- **BalanceTransfer**: Has index, emitted by AToken/vToken contract (internal transfer)
- **ERC20 Transfer**: No index, standard ERC20 Transfer event (e.g., liquidator receiving collateral)

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

*Last updated: 2026-04-13*
*Contributors: Issue #0004, #0007, #0029 investigation teams*
