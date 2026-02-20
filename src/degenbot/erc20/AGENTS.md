# ERC20 Agent Documentation

## Overview

ERC20 token implementation with balance tracking, metadata caching, and native ETH placeholder support.

## Components

**Token Model**
- `erc20.py` - ERC20 token class with balance queries and transfer simulations
  - Database integration: loads/saves from `Erc20TokenTable`
  - Price oracle support via `ChainlinkPriceContract`
  - Async methods: `get_balance_async`, `get_approval_async`, `get_total_supply_async`
  - Batch RPC requests: `get_name_symbol_decimals_batched`
  - Fallback metadata retrieval (name()/NAME(), bytes32 handling)
  - State caching with `BoundedCache`
- `ether_placeholder.py` - Native ETH representation for consistent token interface
  - Limited method support (no async, no approvals, no total supply)
  - Direct ETH balance queries via `w3.eth.get_balance`

**Management**
- `manager.py` - Token registry with address lookup and caching
  - Borg singleton pattern (shared state dict)
  - Threading `Lock` for thread safety
  - Checks internal cache AND `token_registry`
  - Auto-instantiates `EtherPlaceholder` for ETH addresses

## Design Patterns

- Tokens are value objects with immutable metadata (symbol, decimals, name)
- Balance queries go through manager for caching
- ETH placeholder enables uniform handling of ERC20 and native assets
- Borg singleton pattern for global token registry (not true singleton)
- Async/sync dual API pattern
- Two-level caching (manager + registry)
- Inherits from `AbstractErc20Token`
