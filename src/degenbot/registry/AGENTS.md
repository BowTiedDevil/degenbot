# Registry Agent Documentation

## Overview

Global registries for tokens and liquidity pools. Provides lookup by chain ID and address with singleton pattern enforcement.

## Components

**Token Registry**
- `token.py` - TokenRegistry singleton for ERC20 tokens by chain/address

**Pool Registry**
- `pool.py` - PoolRegistry for liquidity pools with Uniswap V4 support

## Module Exports

- `pool_registry` - Pre-initialized `PoolRegistry` instance
- `token_registry` - Pre-initialized `TokenRegistry` instance

## TokenRegistry

Storage: `_all_tokens: dict[tuple[ChainId, ChecksumAddress], Erc20Token]`

Methods:
- `get(token_address: str, chain_id: ChainId) -> Erc20Token | None`
- `add(token_address: str, chain_id: ChainId, token: Erc20Token) -> None`
- `remove(token_address: str, chain_id: ChainId) -> None`

Note: Does not implement AbstractRegistry interface.

## PoolRegistry

Storage: 
- `_all_pools: dict[tuple[ChainId, ChecksumAddress], AbstractLiquidityPool]`
- `_v4_pool_registry: _UniswapV4PoolManagerRegistry`

Methods:
- `get(chain_id, pool_address, pool_id=None) -> AbstractLiquidityPool | None`
- `add(pool, chain_id, pool_address, pool_id=None) -> None`
- `remove(chain_id, pool_address, pool_id=None) -> None`

V4 pools use `pool_id` parameter and are delegated to internal registry with composite key (chain_id, pool_manager_address, HexBytes(pool_id)).

## Error Handling

- `RegistryAlreadyInitialized` - Raised in __init__ if singleton exists
- `DegenbotValueError` - Raised by add() if already registered
- Removal is silent (suppresses KeyError)

## Design Patterns

- Singleton pattern prevents multiple registry instances
- Composite key lookup: (chain_id, address)
- Uniswap V4 singleton pools tracked separately by pool ID
- Address normalization via `get_checksum_address()`
- RegistryAlreadyInitialized exception on duplicate instantiation
