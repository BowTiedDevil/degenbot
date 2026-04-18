# Synthetic Pool State Generator

Generates synthetic Uniswap pool states with known profitable arbitrage conditions for regression testing and benchmarking.

## Overview

The generator produces deterministic pool states that can be used to test arbitrage optimization algorithms without network dependencies. Each generated fixture represents an arbitrage opportunity with known characteristics.

## Key Components

### `types.py`

Configuration dataclasses for pool generation:

- `PoolGenerationConfig` - Base configuration for V2 pools
- `V3PoolGenerationConfig` - V3-specific parameters (tick spacing, liquidity depth)
- `V4PoolGenerationConfig` - V4-specific parameters (hooks address)
- `ArbitrageFixtureConfig` - Complete scenario configuration
- `PriceDiscrepancyConfig` - Controls profit injection between pools

### `pool_generator.py`

Core generation logic:

- `PoolStateGenerator` - Main generator class
- `generate_v2_pool_state()` - Create V2 pool states
- `generate_v3_pool_state()` - Create V3 pool states with tick bitmap
- `generate_v4_pool_state()` - Create V4 pool states
- `generate_profitable_v2_pair()` - Two V2 pools with price difference
- `generate_profitable_v3_pair()` - Two V3 pools with price difference
- `generate_profitable_v4_pair()` - Two V4 pools with price difference
- `generate_profitable_mixed_pair()` - V2 vs V3 arbitrage

### `fixtures.py`

Test fixture management:

- `ArbitrageCycleFixture` - Frozen dataclass representing an arbitrage scenario
- `FixtureFactory` - Factory for creating both simple and stress-test fixtures

## Usage

### Generate a Simple V2 Arbitrage Fixture

```python
from tests.arbitrage.generator import FixtureFactory

factory = FixtureFactory()
fixture = factory.simple_v2_arb_profitable()

# Access pool states
for address, state in fixture.pool_states.items():
    print(f"Pool {address}: reserves={state.reserves_token0}/{state.reserves_token1}")
```

### Generate Random Stress Test Fixtures

```python
from tests.arbitrage.generator import FixtureFactory

factory = FixtureFactory()

# Generate V2 pair with random seed
fixture = factory.random_v2_pair(seed=42, liquidity_depth="medium")

# Generate V3 pair
fixture = factory.random_v3_pair(seed=42, tick_spacing=60)

# Generate multi-pool cycle
fixture = factory.random_multi_pool_cycle(seed=42, num_pools=3)
```

### Save and Load Fixtures

```python
from pathlib import Path
from tests.arbitrage.generator import ArbitrageCycleFixture

# Save
fixture.save(Path("fixtures/my_fixture.json"))

# Load
loaded = ArbitrageCycleFixture.load(Path("fixtures/my_fixture.json"))
```

## Fixture JSON Schema

```json
{
  "id": "simple_v2_arb_profitable",
  "cycle_type": "v2_v2",
  "pool_states": {
    "0x...": {
      "type": "v2",
      "address": "0x...",
      "block": 12345678,
      "reserves_token0": 1000000000000000000,
      "reserves_token1": 2000000000
    }
  },
  "input_token_address": "0x...",
  "expected_optimal_input": 0,
  "expected_profit": 0,
  "profit_tolerance_bps": 10
}
```

## Determinism

All generators use a seed for deterministic output. Same seed always produces the same fixture:

```python
fixture1 = factory.random_v2_pair(seed=42)
fixture2 = factory.random_v2_pair(seed=42)
# fixture1 and fixture2 are identical
```

## Pool State Types

### V2 Pool State

- `reserves_token0` / `reserves_token1` - Reserve amounts
- `address` - Pool contract address
- `block` - Block number

### V3/V4 Pool State

- `liquidity` - Current liquidity
- `sqrt_price_x96` - Current price in Q64.96 format
- `tick` - Current tick
- `tick_bitmap` - Tick bitmap for liquidity distribution
- `tick_data` - Liquidity net/gross at initialized ticks
- `id` (V4 only) - Pool ID bytes

## Liquidity Depths

- `shallow` - ~1 ETH equivalent (10^18)
- `medium` - ~1000 ETH equivalent (10^21)
- `deep` - ~1000000 ETH equivalent (10^24)
