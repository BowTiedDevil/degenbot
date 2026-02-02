# Types Agent Documentation

## Overview

Type aliases, abstract base classes, and concrete implementations for type-safe architecture.

## Components

**Abstract** (`abstract/`)
- Abstract base classes: AbstractLiquidityPool, AbstractArbitrage, Publisher, Subscriber
- Protocol definitions for interfaces

**Concrete** (`concrete/`)
- `BoundedCache` - Size-limited cache with LRU eviction
- `KeyedDefaultDict` - defaultdict with key-aware factory
- `PublisherMixin` - Reusable publisher implementation
- Message types for pub/sub pattern

**Aliases** (`aliases.py`)
- Type aliases: BlockNumber, ChainId, Address, etc.
- PEP 695 syntax for vague base types

**Caching** (`caching/`)
- Cache implementation utilities

**Collections** (`collections/`)
- Specialized collection types

**Messaging** (`messaging/`)
- Publisher/subscriber message types

## Design Patterns

- Abstract classes define interfaces via Protocol
- Mixins provide shared functionality
- Type aliases improve code clarity
- WeakSet for subscriber references prevents memory leaks
