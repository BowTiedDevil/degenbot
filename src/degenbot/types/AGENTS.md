# Types Agent Documentation

## Overview

Type aliases, abstract base classes, and concrete implementations for type-safe architecture.

## Components

**Abstract** (`abstract/`)
- `AbstractLiquidityPool` - Base class for liquidity pools with comparison methods
- `AbstractArbitrage` - Base class for arbitrage strategies
- `AbstractErc20Token` - ERC20 token interface with comparison methods
- `AbstractExchangeDeployment` - Exchange deployment dataclass
- `AbstractPoolManager` - Pool manager base with singleton pattern
- `AbstractPoolState` - Immutable pool state dataclass (frozen, slots)
- `AbstractManager`, `AbstractRegistry` - Manager and registry base classes
- `AbstractSimulationResult`, `AbstractPoolUpdate`, `AbstractTransaction` - Additional base classes

**Concrete** (`concrete.py`)
- `BoundedCache` - Size-limited cache with LRU eviction
- `KeyedDefaultDict` - defaultdict with key-aware factory
- `PublisherMixin` - Reusable publisher implementation
- `Publisher`, `Subscriber` - Protocols for pub/sub interfaces
- `AbstractPublisherMessage`, `PoolStateMessage`, `TextMessage` - Message types

**Aliases** (`aliases.py`)
- `BlockNumber` - Block number as int
- `ChainId` - Chain ID as int
- `Tick` - Price tick as int
- `Word` - EVM word as int
- PEP 695 `type` syntax for type aliases (Python 3.12+)

## Design Patterns

- Abstract classes define interfaces via Protocol
- `AbstractPoolManager` uses `WeakValueDictionary` for singleton instances
- `AbstractLiquidityPool` and `AbstractErc20Token` implement rich comparison methods
- `AbstractPoolState` uses dataclass with `slots=True, frozen=True`
- Mixins provide shared functionality
- Type aliases improve code clarity
- WeakSet for subscriber references prevents memory leaks

## Module Exports

`__init__.py` exports:
- `BoundedCache`
- `KeyedDefaultDict`

Other classes must be imported from submodules directly.
