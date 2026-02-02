# ERC20 Agent Documentation

## Overview

ERC20 token implementation with balance tracking, metadata caching, and native ETH placeholder support.

## Components

**Token Model**
- `erc20.py` - ERC20 token class with balance queries and transfer simulations
- `ether_placeholder.py` - Native ETH representation for consistent token interface

**Management**
- `manager.py` - Token registry with address lookup and caching

## Design Patterns

- Tokens are value objects with immutable metadata (symbol, decimals, name)
- Balance queries go through manager for caching
- ETH placeholder enables uniform handling of ERC20 and native assets
- Manager uses singleton pattern for global token registry
