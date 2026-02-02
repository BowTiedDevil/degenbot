# Connection Agent Documentation

## Overview

Web3 connection management for sync and async operations. Manages multiple chain connections with retry logic and connection optimization.

## Components

**Connection Managers**
- `connection_manager.py` - Synchronous Web3 connection management
- `async_connection_manager.py` - Asynchronous Web3 connection management

**Convenience Functions**
- `get_web3()` / `set_web3()` - Get/set default sync connection
- `get_async_web3()` / `set_async_web3()` - Get/set default async connection

## Design Patterns

- Singleton managers accessible at module level
- Multi-chain support via chain ID mapping
- Tenacity retry for connection establishment
- ujson for fast RPC response parsing
- Connection optimization via provider configuration
