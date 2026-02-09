---
title: Pool CLI Commands
category: cli
tags:
  - state-management
  - uniswap-v2
  - uniswap-v3
  - uniswap-v4
  - aerodrome
  - pancakeswap
  - sushiswap
  - swapbased
  - liquidity
related_files:
  - ../../src/degenbot/cli/pool.py
  - ../../src/degenbot/cli/exchange.py
  - ../../src/degenbot/database/models/pools.py
  - ../../src/degenbot/database/models/base.py
  - ../../src/degenbot/uniswap/v3_liquidity_pool.py
  - ../../src/degenbot/uniswap/v4_liquidity_pool.py
complexity: complex
---

## Overview

The Pool CLI provides commands for managing liquidity pool metadata and tracking liquidity positions across multiple DEX versions (Uniswap 2, 3, and 4) on Base and Ethereum mainnet. The main command `pool update` fetches blockchain events for pool discovery and liquidity updates, synchronizing database with current pool states.

## Background: Pool Architecture

### Uniswap V2 Architecture

V2 pools use a **constant product AMM** (x*y=k) formula. All liquidity is distributed evenly across entire price range. Pools are created by factories with a `PairCreated` event containing token addresses and pool address.

### Uniswap V3 Architecture

V3 introduces **concentrated liquidity** - liquidity providers can allocate capital to custom price ranges using tick boundaries. This requires tracking tick-level liquidity with:

- **Tick bitmap**: 256-bit words mapping initialized tick positions
- **Liquidity mapping**: Net and gross liquidity at each tick for price traversal
- **Tick spacing**: Constraints on allowed tick positions per fee tier

Pools emit `Mint` and `Burn` events for liquidity changes.

### Uniswap V4 Architecture

V4 uses a **centralized pool manager** (instead of per-pool contracts) with hooks for custom logic:

- **PoolManager**: Single contract managing all pools
- **PoolKey**: Keccak256 hash identifying pools (currency0, currency1, fee, tickSpacing, hooks)
- **Hooks**: Customizable behavior at pool boundaries
- **ModifyLiquidity**: Single event type for liquidity operations

V4 uses separate `ManagedPool` database tables with pool manager references.

## Commands

All CLI commands are implemented in [`src/degenbot/cli/pool.py`](../../src/degenbot/cli/pool.py) and [`src/degenbot/cli/exchange.py`](../../src/degenbot/cli/exchange.py).

### `degenbot pool update`

Update pool metadata and liquidity positions for all activated exchanges.

```bash
degenbot pool update [--chunk SIZE] [--to-block BLOCK]
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

1. **Identify active chains**: Queries database for chains with active exchanges
2. **Determine update range**: Starts from `min(last_update_block + 1)` across exchanges
3. **Process in chunks**: Iteratively processes blocks up to `chunk_size`
4. **Track progress**: Displays progress bar showing blocks processed
5. **Skip up-to-date chains**: If no new blocks exist since last update, skips

#### Example Usage

```bash
degenbot pool update --to-block "latest:-128"
degenbot pool update --chunk 5000
```

### `degenbot exchange activate`

Activate an exchange for pool tracking. Creates database entry if not exists.

```bash
degenbot exchange activate [base_aerodrome_v2 | base_aerodrome_v3 | base_pancakeswap_v2 | base_pancakeswap_v3 | base_sushiswap_v2 | base_sushiswap_v3 | base_swapbased_v2 | base_uniswap_v2 | base_uniswap_v3 | base_uniswap_v4 | ethereum_pancakeswap_v2 | ethereum_pancakeswap_v3 | ethereum_sushiswap_v2 | ethereum_sushiswap_v3 | ethereum_uniswap_v2 | ethereum_uniswap_v3 | ethereum_uniswap_v4]
```

V4 exchanges additionally create a PoolManagerTable entry.

### `degenbot exchange deactivate`

Deactivate an exchange (pools not updated).

```bash
degenbot exchange deactivate [exchange_name]
```

## Supported Exchanges

### Base Mainnet
- Aerodrome V2, V3
- Pancakeswap V2, V3
- Sushiswap V2, V3
- Swapbased V2
- Uniswap V2, V3, V4

### Ethereum Mainnet
- Pancakeswap V2, V3
- Sushiswap V2, V3
- Uniswap V2, V3, V4

## Fee Structure

### V2 Fees
Fixed fee per pool:
- **Uniswap V2**: 0.3% (3/1000)
- **Sushiswap V2**: 0.3% (3/1000)
- **Swapbased V2**: 0.3% (3/1000)
- **Pancakeswap V2**: 0.25% (25/10000)
- **Aerodrome V2**: Variable (queried from factory via `getFee()`), includes stable pair flag

### V3 Fees
Tiered fee structure (basis points, denominator = 1,000,000):
- **500 bps** (0.05%) - Stable pairs
- **3000 bps** (0.3%) - Standard pairs
- **10000 bps** (1.0%) - Exotic pairs

### V4 Fees
Dynamic fee support:
- Range: 0 to 1,000,000 bps (0% to 100%)
- Specified per pool in PoolKey

## Pool Discovery Events

### V2: PairCreated Event

**Event Hash**: `0x0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9`

Used by: Uniswap V2, Pancakeswap V2, Sushiswap V2, Swapbased V2

### V3: PoolCreated Event

**Event Hash**: `0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118`

Used by: Uniswap V3, Pancakeswap V3, Sushiswap V3, Aerodrome V3. **Aerodrome V3**: Fetches fee from factory after discovery.

### V4: PoolCreated Event

**Event Hash**: `0xdd466e674ea557f56295e2d0218a125ea4b4f0f6f3307b95f85e6110838d6438`

## Liquidity Update Processing

### V3 Mint Event

**Event Hash**: `0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde`

**Processing**: Adds `liquidity_gross` and `liquidity_net` across tick range. Updates tick bitmap at word boundaries.

### V3 Burn Event

**Event Hash**: `0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c`

**Processing**: Subtracts `liquidity_gross` and `liquidity_net` across tick range. Deletes position if balance reaches zero.

### V4 ModifyLiquidity Event

**Event Hash**: `0xf208f4912782fd25c7f114ca3723a2d5dd6f3bcc3ac8db5af63baa85f711d5ec`

**Processing**: Single event with signed `liquidityDelta` (positive for add, negative for remove). Updates managed pool tables.

## Mock Pool Helpers

Mock pool classes (`MockV3LiquidityPool`, `MockV4LiquidityPool`) are lightweight pool implementations used to simulate liquidity updates and validate tick calculations without expensive operations (state locking, notifications, caching).

**Simplifications**:
- No-op `_state_lock`, empty `_notify_subscribers()`, `_invalidate_range_cache_for_ticks()`
- `_initial_state_block` set to `MAX_UINT256` to skip in-range modifications
- Full (non-sparse) liquidity mapping

**Usage**: Load state from database → create mock → process events chronologically → export validated mappings back to database.

## Data Model Updates

All database models are defined in [`src/degenbot/database/models/pools.py`](../../src/degenbot/database/models/pools.py) and [`src/degenbot/database/models/base.py`](../../src/degenbot/database/models/base.py):

| Table | Fields Updated | Notes |
|-------|----------------|-------|
| `ExchangeTable` | `last_update_block` | After each chunk completes |
| `Erc20TokenTable` | New rows created | For token0/token1/currency0/currency1 |
| `LiquidityPoolTable` | New rows created | V2/V3 pool metadata from factory events |
| `UniswapV4PoolTable` | New rows created | V4 pool metadata from manager events |
| `PoolManagerTable` | New rows created | V4 manager metadata (on activate) |
| `LiquidityPositionTable` | `liquidity_net`, `liquidity_gross` | Upsert from V3 liquidity events |
| `InitializationMapTable` | `bitmap` | Upsert from V3 tick bitmap updates |
| `ManagedPoolLiquidityPositionTable` | `liquidity_net`, `liquidity_gross` | Upsert from V4 liquidity events |
| `ManagedPoolInitializationMapTable` | `bitmap` | Upsert from V4 tick bitmap updates |

## Algorithm Details

### Chunk Processing

Blocks are processed in chunks to limit memory usage and enable incremental commits:

1. Calculate `working_end_block` as minimum of:
   - `last_block` (target block)
   - `working_start_block + chunk_size - 1`
   - All `exchange.last_update_block` values ahead of current chunk

2. Update exchanges where `last_update_block is None` or `last_update_block + 1 == working_start_block`

3. Commit changes and advance `working_start_block = working_end_block + 1`

4. Repeat until `working_end_block == last_block`

### Event Ordering

Events are processed in block number, then log index order to ensure chronological processing within blocks:

```python
sorted(
    all_events, 
    key=operator.itemgetter(
        "blockNumber", 
        "logIndex"
    )
)
```

Invariants enforced by assertions:
- New event block ≥ last update block
- Same block events must have increasing log index

### SQLite Variable Limits

Upsert operations are chunked to stay below SQLite's 32,766 variable limit per batch statement:

- **Liquidity positions**: 30,000 rows per chunk (4 keys/row)
- **Initialization maps**: 30,000 rows per chunk (3 keys/row)

Zero-bitmap words (all ticks uninitialized) are excluded from upserts to reduce database size.

## Configuration

The command uses Web3 connections from degenbot config file. Each active chain must have an RPC endpoint configured.

### Required Config

```yaml
rpc:
  1: https://mainnet.example.com  # Ethereum mainnet
  8453: https://base.example.com    # Base mainnet
```

## Dependencies

- **Database**: SQLAlchemy ORM
- **Blockchain**: Web3.py for RPC calls
- **Math**: Uniswap V3/V4 libraries (tick bitmap, tick math, liquidity math)
