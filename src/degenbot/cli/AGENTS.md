# CLI Agent Documentation

## Overview

CLI command implementations for Aave V3, liquidity pools, exchanges, and database operations.

## Commands

**Aave V3**
- `aave.py` - Commands: `update`, `activate`, `deactivate`
- See `../../../docs/cli/aave.md` for detailed docs
- Environment variables for debug: `DEGENBOT_VERBOSE_ALL`, `DEGENBOT_VERBOSE_USERS`, `DEGENBOT_VERBOSE_TX`

**Aave Update Options**:
- `--chunk` / `chunk_size`: Block chunk size for processing (default: 10,000)
- `--to-block`: Target block identifier with offset support (default: `latest:-64`)
- `--verify`: Verify positions at block boundaries (default: True)
- `--one-chunk`: Stop after first chunk (default: False)
- `--no-progress-bar`: Disable progress bars (default: False)

**Liquidity Pools**
- `pool.py` - Commands: `update`
- `exchange.py` - Commands: `activate`, `deactivate`
- See `../../../docs/cli/pool.md` for detailed docs

**Pool Update Options**:
- `--chunk`: Blocks per commit batch (default: 10,000)
- `--to-block`: End block with offset support (e.g., 'latest:-64')

**Database**
- `database.py` - Commands: `backup`, `reset`, `upgrade`, `compact`
- See `../../../docs/cli/database.md` for detailed docs

## Supported Exchanges

**Base chain**:
- Aerodrome V2/V3
- Pancakeswap V2/V3
- Sushiswap V2/V3
- SwapBased V2
- Uniswap V2/V3/V4

**Ethereum mainnet**:
- Pancakeswap V2/V3
- Sushiswap V2/V3
- Uniswap V2/V3/V4

## Related Files

- `aave.py` - Aave CLI commands with event processing
- `database.py` - Database commands
- `exchange.py` - Exchange commands
- `pool.py` - Liquidity pool commands with V3/V4 liquidity tracking
- `utils.py` - CLI utilities (Web3 connection creation)

## Key Implementation Details

**Aave Processing**:
- Transaction-level event processing with `TransactionContext`
- Event types: SCALED_TOKEN_MINT, SCALED_TOKEN_BURN, SCALED_TOKEN_BALANCE_TRANSFER, RESERVE_DATA_UPDATED, USER_E_MODE_SET, UPGRADED, DISCOUNT_PERCENT_UPDATED
- GHO integration with discount mechanisms and stkAAVE balance tracking
- Token revision tracking (v1 through v6) with different math libraries

**Pool Processing**:
- `POOL_UPDATER` registry mapping chain/exchange to updater functions
- V3/V4 liquidity tracking via Mint/Burn/ModifyLiquidity events
- Tick bitmap management for concentrated liquidity
- Mock pools for simulation: `MockV3LiquidityPool`, `MockV4LiquidityPool`
