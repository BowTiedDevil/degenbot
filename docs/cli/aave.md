---
title: Aave CLI Commands
category: cli
tags:
  - state-management
  - liquidity
related_files:
  - ../../src/degenbot/cli/aave.py
  - ../../src/degenbot/database/models/aave.py
complexity: complex
---

# Aave CLI Commands

## Overview

The Aave CLI provides commands for managing Aave V3 markets and updating position data. The main command `aave_update` fetches blockchain events, processes them, and synchronizes the database with current user positions (collateral and debt).

## Background: Aave V3 Architecture

Aave V3 uses **scaled balances** for tracking user positions:

- **aTokens** represent collateral deposits, scaled by a liquidity index
- **vTokens** (variable debt tokens) represent borrowed amounts, scaled by a borrow index
- Scaling indexes grow over time as interest accrues, ensuring users earn/pay proportional interest

When events occur (supply, withdraw, borrow, repay, transfer), the database stores scaled balances. The actual balance at any point is calculated as:

```
actual_balance = scaled_balance * index
```

The system tracks events from:
- **Pool contract**: Reserve configuration, E-Mode changes
- **aToken contracts**: Collateral operations (Mint/Burn/Transfer)
- **vToken contracts**: Borrow/repay operations (Mint/Burn)
- **GHO vToken**: Special debt token with discount mechanism for GHO borrowing

## GHO Token Support

GHO is Aave's stablecoin with discounted borrowing based on stkAAVE holdings. The CLI tracks GHO positions with revision-specific logic:

### GHO Tables

- **`AaveGhoTokenTable`**: Discount token address and rate strategy
- **`AaveV3UsersTable.gho_discount`**: User's discount percentage

### Revision Differences

- **Revision 1**: Uses `_accrueDebtOnAction()` for discount accounting
- **Revision 2**: Uses `_get_discounted_balance()` with enhanced balance calculation
- **Revision 4+**: Discount mechanism deprecated, uses different rounding for mint operations

### GHO V4+ Rounding Differences

**Critical**: GHO V4+ uses **different rounding** than standard vTokens V4+:

| Operation | Standard vToken V4+ | GHO vToken V4+ |
|-----------|---------------------|----------------|
| BORROW (mint) | `ray_div_ceil` (ceiling) | `ray_div_floor` (floor) |
| REPAY (burn) | `ray_div_floor` (floor) | `ray_div_floor` (floor) |

This difference exists because GHO deprecated the discount mechanism in revision 4, which changed the rounding requirements in the Pool contract.

When processing GHO BORROW events, the scaled amount must be pre-calculated
from the original borrow amount (extracted from the BORROW event) using
`calculate_mint_scaled_amount()` before being applied to the Mint event.
This rounding behaviour now lives in the Rust `degenbot-aave-updater` core
(see the *Writer implementation* section below); the Python
`TokenProcessorFactory` that previously exposed it was retired with the
rest of the Python enrichment/processing pipeline.

## Offline Position Calculation

The `aave_update` command is designed to rebuild a complete database of collateral and debt positions by retrieving chronological events from several Aave contracts and decoding the underlying amounts that are stored in smart contract storage. This event-driven approach allows for fast offline calculation of positions without making any RPC calls to query live contract state. Once the database is synchronized, all position calculations can be performed locally using the stored scaled balances and index values.

## Commands

All CLI commands are implemented in [`src/degenbot/cli/aave.py`](../../src/degenbot/cli/aave.py).

### `degenbot aave update`

Update positions for all active Aave V3 markets by processing blockchain events.

```bash
degenbot aave update [--chunk SIZE] [--to-block BLOCK]
```

#### Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `--chunk` | 10,000 | Maximum number of blocks to process per database commit |
| `--to-block` | `latest:-64` | Last block in update range. Format: `TAG[:OFFSET]` |

#### Block Identifiers

Valid block tags: `earliest`, `finalized`, `safe`, `latest`, `pending`

Examples:
- `latest` - Latest block
- `latest:-64` - 64 blocks before chain tip (default, ensures finality)
- `safe:128` - 128 blocks after last safe block
- `12345678` - Specific block number

#### Behavior

1. **Identify active markets**: Queries database for active Aave V3 markets on all chains
2. **Determine update range**: Starts from `last_update_block + 1` for each market
3. **Process in chunks**: Iteratively processes blocks up to `chunk_size`, committing after each chunk
4. **Track progress**: Displays progress bar showing blocks processed
5. **Skip up-to-date chains**: If no new blocks exist since last update, skips that chain

#### Example Usage

```bash
# Update all active markets to 128 blocks before latest
degenbot aave update --to-block "latest:-128"

# Update using smaller chunks for slower machines
degenbot aave update --chunk 5000

# Update to a specific block for historical analysis
degenbot aave update --to-block "18900000"
```

### `degenbot aave position show`

Show a user's position in a specific Aave market.

```bash
degenbot aave position show <ADDRESS> [--market MARKET] [--chain-id CHAIN_ID]
```

Displays collateral and debt positions for the given address.

### `degenbot aave position risk`

Show risk parameters for a user's position.

```bash
degenbot aave position risk <ADDRESS> [--market MARKET] [--chain-id CHAIN_ID]
```

Displays health factor, liquidation threshold, and LTV information.

### `degenbot aave market show`

Show market state and configuration.

```bash
degenbot aave market show [--chain-id CHAIN_ID] [--name NAME]
```

### `degenbot aave activate`

Activate an Aave market for tracking.

```bash
degenbot aave activate ethereum_aave_v3
```

Only activated markets are included in `aave_update` runs.

### `degenbot aave deactivate`

Deactivate an Aave market (positions not updated).

```bash
degenbot aave deactivate ethereum_aave_v3
```

## Event Processing Details

### Reserve Initialization (`ReserveInitialized`)

Tracked from PoolConfigurator contract. Creates new asset entry with:
- Underlying ERC20 token address
- aToken and vToken addresses
- Token implementation revisions (for encoding compatibility)

**Data models updated**: `Erc20TokenTable`, `AaveV3AssetsTable`

### Reserve Data Update (`ReserveDataUpdated`)

Emitted from Pool when interest rates change. Updates:
- `liquidity_rate`: Current supply rate (RAY precision)
- `borrow_rate`: Current variable borrow rate (RAY precision)
- `liquidity_index`: Index for converting collateral scaled balances
- `borrow_index`: Index for converting debt scaled balances
- `last_update_block`: Block of last rate update

**Data model updated**: `AaveV3AssetsTable`

### User E-Mode Set (`UserEModeSet`)

Emitted when user changes their efficiency mode category.

**Data model updated**: `AaveV3UsersTable.e_mode`

### Scaled Token Mint (`Mint`)

Mint events originate from three sources, identified by comparing `value` and `balanceIncrease`:

```
if value == balanceIncrease:     # _transfer - skip (BalanceTransfer handles this)
elif balanceIncrease > value:    # _burnScaled - interest accrual
else:                             # _mintScaled - user action (supply/borrow)
```

**User Operations:**

| Operation | Trigger |
|-----------|---------|
| `DEPOSIT` | aToken _mintScaled |
| `WITHDRAW` | aToken _burnScaled (interest > withdrawal) |
| `BORROW` | vToken _mintScaled |
| `REPAY` | vToken _burnScaled (interest > repayment) |
| `GHO BORROW` | GHO vToken mint |
| `GHO REPAY` | GHO vToken burn |

**Processing:**
- `_mintScaled`: `amount = value - balanceIncrease`, `amountScaled = ray_div(amount, index)`
- `_burnScaled`: Add `event_value` directly as interest
- All sources create user/position if needed

**Data models**: `AaveV3UsersTable`, position tables, `AaveGhoTokenTable` for GHO

### Scaled Token Burn (`Burn`)

Burn events always follow `_burn(amountScaled)` storage reduction.

**Processing:**
- Event `value` = `amount - balanceIncrease` (net after interest)
- Reconstruct: `amount = event_value + balance_increase`
- Convert: `amountScaled = ray_div(amount, index)`
- Subtract `amountScaled` from position balance
- Delete position if balance reaches zero

**GHO vToken**: Uses `_process_gho_debt_burn()` with revision-specific logic (1 or 2).

### Balance Transfer (`BalanceTransfer`)

Only occurs for aTokens (collateral). Transfers scaled amount directly:
- Decrements sender's collateral balance
- Increments recipient's collateral balance (creates user/position if needed)
- Deletes sender's position if balance reaches zero

**Data models updated**: `AaveV3UsersTable`, `AaveV3CollateralPositionsTable`

### Token Upgrade (`Upgraded`)

When aToken or vToken implementation changes:
- Detects which token type (aToken or vToken)
- Queries new implementation for revision number
- Updates revision in `AaveV3AssetsTable`

**Data model updated**: `AaveV3AssetsTable.a_token_revision` or `AaveV3AssetsTable.v_token_revision`

### GHO Discount Token Updated (`DiscountTokenUpdated`)

Emitted when the discount token for GHO vToken changes.

**Data model updated**: `AaveGhoTokenTable.v_gho_discount_token`

### GHO Discount Rate Strategy Updated (`DiscountRateStrategyUpdated`)

Emitted when the discount rate strategy for GHO vToken changes. The strategy calculates discount percentages based on user's GHO debt and discount token balances.

**Data model updated**: `AaveGhoTokenTable.v_gho_discount_rate_strategy`

## Error Handling & Validation

### Pre-update Checks

1. **Invalid block tag**: Raises `ValueError` if `to_block` tag not in valid set
2. **Future block**: Raises `ValueError` if `to_block` ahead of current chain tip
3. **No new blocks**: Logs message and skips chain if `start_block >= end_block`

### Event Processing Validation

1. **Asset existence**: Asserts asset exists in DB before processing events
2. **User existence**: Creates user entry if not found (for collateral operations)
3. **Position tracking**: Maintains invariant that position exists when processing operations
4. **Non-negative balances**: Asserts balances never go negative
5. **Revision compatibility**: Raises `ValueError` for unsupported aToken/vToken revisions

### Balance Verification

After each block, performs **balance checks** on modified users:
- Calls `scaledBalanceOf(address)` on contract
- Compares against stored `balance` value
- Raises assertion error on mismatch with detailed context

**GHO Discount Verification**:
- Calls `getDiscountPercent(address)` on GHO vToken contract
- Compares against stored `gho_discount` value in `AaveV3UsersTable`
- Raises assertion error on mismatch with detailed context

## Algorithm Details

### Chunk Processing

The update processes blocks in chunks to limit memory usage and enable incremental commits:

```python
while working_start_block <= last_block:
    # Calculate end of current chunk (minimum of constraints)
    working_end_block = min(
        last_block,
        working_start_block + chunk_size - 1,
        market.last_update_block for each market (if ahead)
    )

    # Update all markets ready for this chunk
    for market in markets_needing_update(start == market.last_update_block + 1):
        update_aave_v3_market(start, end, market)

    # Commit changes
    for market in updated_markets:
        market.last_update_block = working_end_block
    session.commit()

    # Advance to next chunk
    working_start_block = working_end_block + 1
```

### Market Selection Logic

A market is updated in a chunk if:
- `market.last_update_block is None` (never updated), OR
- `market.last_update_block + 1 == working_start_block` (ready for next segment)

This allows markets at different block heights to be synchronized gradually.

### Event Ordering

Events are processed in block number, then log index order:
```python
sorted(all_events, key=operator.itemgetter("blockNumber", "logIndex"))
```

This ensures chronological processing within each block.

## Token Revision Libraries

The code supports multiple Aave V3 token revisions. The wad/ray math (scaled-balance
multiplication with rounding) is **Rust-owned** (`degenbot-evm-math::wad_ray_math`,
exposed to the updater via `degenbot-aave::updater::processors`). The rounding
modes mirror the Solidity `WadRayMathLibrary`:

| Rounding Mode | Usage |
|--------------|-------|
| Default (no arg) | Half-up rounding (revisions 1-3) |
| `Rounding.FLOOR` | Round down (aToken mint rev 4+, vToken burn rev 4+) |
| `Rounding.CEIL` | Round up (aToken burn rev 4+, vToken mint rev 4+) |

### Rounding Functions by Revision

**All revisions**: `ray_div(a, b, rounding=...)` with the rounding mode
(`FLOOR` / `CEIL` / half-up default) — the Rust `processors` module selects
the mode per token revision + operation.

- `Rounding.FLOOR` - Round down
- `Rounding.CEIL` - Round up
- Default (no rounding arg) - half-up rounding

### Rounding by Token Type and Operation

| Token Type | Revision | Mint (Supply/Borrow) | Burn (Withdraw/Repay) |
|------------|----------|---------------------|---------------------|
| aToken | 1-3 | `ray_div` (half-up) | `ray_div` (half-up) |
| aToken | 4+ | `ray_div` (FLOOR) | `ray_div` (CEIL) |
| vToken | 1-3 | `ray_div` (half-up) | `ray_div` (half-up) |
| vToken | 4+ | `ray_div` (CEIL) | `ray_div` (FLOOR) |
| GHO vToken | 4+ | `ray_div` (FLOOR) ⚠️ | `ray_div` (FLOOR) |

⚠️ **Important**: GHO V4+ uses `ray_div_floor` for BORROW (mint), unlike standard vTokens V4+ which use `ray_div_ceil`.

## Writer implementation

The Aave V3 writer is **Rust-owned** (`degenbot-aave` core crate). The per-market chunk loop, RPC fetch+decode, DB writes, the per-chunk transaction, and the on-chain-truth verification all live in the Rust core, driven from `degenbot aave update` via the `run_aave_update` PyO3 seam (a thin driver shell). The former Python writer pipeline (`update_aave_market`, `event_handlers._process_*`, `transaction_processor`/`operations_parser`/`token_processor`, `db_*.py`, the `verify_*` Python invariants) was retired by the §4.2 cutover (task `CZM7TI`) after the Rust path was proven GREEN to the live chain tip with full verification. The `Event Processing Details` and `Algorithm Details` sections above describe domain behavior that remains accurate; the implementation now lives in `rust/crates/degenbot-aave/`.

## Configuration

The command uses Web3 connections from the degenbot config file. Each active chain must have an RPC endpoint configured.

### Required Config

```toml
[rpc]
1 = "https://mainnet.example.com"  # Ethereum mainnet
# ... other chain IDs
```

## Environment Variables

### General

| Variable | Values | Description |
|----------|--------|-------------|
| `DEGENBOT_DEBUG` | `1`, `true`, `yes` | Enable debug-level logging output |
| `DEGENBOT_DEBUG_FUNCTION_CALLS` | `1`, `true`, `yes` | Enable function call trace logging |
| `DEGENBOT_COVERAGE` | `1` | Enable CLI code coverage tracking (dev use) |

## Dependencies

- **Database**: SQLAlchemy ORM (see [`src/degenbot/database/models/aave.py`](../../src/degenbot/database/models/aave.py))
- **Blockchain**: Web3.py for RPC calls
- **Math**: Rust `degenbot-evm-math::wad_ray_math` for scaled balance calculations with rounding mode support (the former Python `aave/libraries/` package was retired)
- **Logging**: Click for CLI output, tqdm for progress bars
- **Writer**: Rust `degenbot-aave-updater` core crate (the Python enrichment/processing pipeline was retired)

## Solidity Reference

The CLI interacts with Aave V3 contracts. Key implementation details in [`rust/crates/degenbot-aave/src/updater/`](../../rust/crates/degenbot-aave/src/updater/) + [`degenbot-evm-math`](../../rust/crates/degenbot-evm-math/src/wad_ray_math.rs):

### Scaled Balance Pattern

```solidity
// From ScaledBalanceTokenBase.sol
function _mintScaled(...) {
  uint256 amountScaled = amount.rayDiv(index);
  _mint(onBehalfOf, amountScaled.toUint128());  // Storage change
  emit Mint(..., amount + balanceIncrease, balanceIncrease, index);  // Event
}
```

Events are notifications only. Actual storage changes happen in `_mint()` and `_burn()` calls using `amountScaled`.

### Rounding

Solidity uses half-up rounding: `(a * b + HALF_RAY) / RAY`

The Rust port (`degenbot-evm-math::wad_ray_math`) matches this exactly for correct balance synchronization. The `processors` module in `degenbot-aave` selects `FLOOR`, `CEIL`, or half-up rounding per token revision + operation.

### Event Structure

Mint/Burn events include:
- `value`: Emitted amount (not storage amount)
- `balanceIncrease`: Interest since last action
- `index`: Current liquidity/borrow index

CLI derives storage amounts: `amountScaled = ray_div(user_amount, index)`
