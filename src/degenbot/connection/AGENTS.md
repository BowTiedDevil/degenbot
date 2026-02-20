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

## Module Exports

`__all__` exports:
- `async_connection_manager`
- `connection_manager`
- `get_async_web3`
- `get_web3`
- `set_async_web3`
- `set_web3`

## Manager API

Both managers have identical APIs:
- `register_web3(w3, *, optimize=True)` - registers connection with optional optimization
- `get_web3(chain_id)` - retrieves connection by chain ID
- `set_default_chain(chain_id)` - sets default for convenience functions
- `default_chain_id` property - returns current default (raises if unset)

## Optimization

`_fast_decode_rpc_response()` - monkey-patches ujson JSON decoding for 10x+ speedup:
- Clears ALL middleware: `w3.middleware_onion.clear()`
- Replaces JSON decoder: `w3.provider.decode_rpc_response = _fast_decode_rpc_response`

## Retry Configuration

- Stop after 10 second delay
- Exponential backoff with jitter
- Retries only if `is_connected` returns False

## Error Handling

- `DegenbotValueError` - chain not registered
- `DegenbotValueError` - Web3 not connected
- `DegenbotValueError` - default chain not set

## Design Patterns

- Singleton managers accessible at module level
- Multi-chain support via chain ID mapping
- Tenacity retry for connection establishment
- ujson for fast RPC response parsing
- Identical async/sync method signatures
- Uses `ChainId` type alias
