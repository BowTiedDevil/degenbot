# Registry Agent Documentation

## Overview

Global registries for tokens and liquidity pools. Provides lookup by chain ID and address with singleton pattern enforcement.

## Components

**Token Registry**
- `token.py` - TokenRegistry singleton for ERC20 tokens by chain/address

**Pool Registry**
- `pool.py` - PoolRegistry for liquidity pools with Uniswap V4 support

## Design Patterns

- Singleton pattern prevents multiple registry instances
- Composite key lookup: (chain_id, address)
- Uniswap V4 singleton pools tracked separately by pool ID
- RegistryAlreadyInitialized exception on duplicate instantiation
