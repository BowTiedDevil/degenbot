"""
Arbitrage testing framework.

Provides synthetic pool state generation, fixtures, and regression testing
for arbitrage optimization algorithms.
"""

from tests.arbitrage.generator import (
    ArbitrageCycleFixture,
    ArbitrageFixtureConfig,
    FixtureFactory,
    PoolGenerationConfig,
    PoolStateGenerator,
    PriceDiscrepancyConfig,
    V3PoolGenerationConfig,
    V4PoolGenerationConfig,
)
from tests.arbitrage.presets import (
    ALL_FIXTURES,
    SIMPLE_FIXTURES,
    FixtureSuite,
    generate_fixture_by_name,
    load_fixture_by_name,
)

__all__ = (
    # Presets
    "ALL_FIXTURES",
    "SIMPLE_FIXTURES",
    # Generator
    "ArbitrageCycleFixture",
    "ArbitrageFixtureConfig",
    "FixtureFactory",
    "FixtureSuite",
    "PoolGenerationConfig",
    "PoolStateGenerator",
    "PriceDiscrepancyConfig",
    "V3PoolGenerationConfig",
    "V4PoolGenerationConfig",
    "generate_fixture_by_name",
    "load_fixture_by_name",
)
