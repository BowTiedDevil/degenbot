"""
Prototype: Bot as Test Harness

This file demonstrates how Bot could be refactored to serve as a robust test harness.
Shows both "quick win" (data override) and "clean architecture" (data source) approaches.
"""

import json
import pathlib
from collections.abc import Callable
from dataclasses import dataclass
from typing import Protocol

from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.erc20.erc20 import Erc20Token

# =============================================================================
# PATTERN 1: Data Override (Quick Win)
# =============================================================================


@dataclass
class CurvePoolTestData:
    """Pre-fetched data for constructing a Curve pool in tests."""

    tokens: tuple  # tuple[Erc20Token, ...]
    balances: tuple[int, ...]
    a_coefficient: int
    fee: int
    admin_fee: int
    state_block: int = 18_000_000

    # Optional fetcher overrides (if None, create deterministic defaults)
    timestamp_fetcher: Callable[[int], int] | None = None
    virtual_price_fetcher: Callable[[int], int] | None = None
    base_virtual_price_fetcher: Callable[[int], int] | None = None

    def get_fetchers(self) -> dict:
        """Get fetcher kwargs for pool construction."""
        return {
            "timestamp_fetcher": self.timestamp_fetcher or (lambda block: 1700000000),
            "virtual_price_fetcher": self.virtual_price_fetcher or (lambda block: 10**18),
            "base_virtual_price_fetcher": self.base_virtual_price_fetcher or (lambda block: 10**18),
        }


# How it would look in Bot:
class BotWithDataOverride:
    """Extended Bot with data override support."""

    def build_curve_pool(
        self,
        address: str,
        *,
        chain_id: int | None = None,
        test_data: CurvePoolTestData | None = None,  # NEW
        **kwargs,
    ):
        """Build a Curve pool from chain data or test data."""

        if test_data is not None:
            # Skip all RPC calls, use provided test data
            return self._build_curve_pool_from_test_data(address, test_data, chain_id)

        # Existing production flow
        return self._build_curve_pool_from_chain(address, chain_id, **kwargs)

    def _build_curve_pool_from_test_data(
        self,
        address: str,
        data: CurvePoolTestData,
        chain_id: int | None,
    ):
        """Construct pool from test data. No RPC calls."""

        return CurveStableswapPool(
            address=address,
            tokens=data.tokens,
            a_coefficient=data.a_coefficient,
            fee=data.fee,
            admin_fee=data.admin_fee,
            balances=data.balances,
            state_block=data.state_block,
            **data.get_fetchers(),  # Inject deterministic fetchers
        )

    def _build_curve_pool_from_chain(self, address: str, chain_id: int, **kwargs):
        """Existing production flow - all the RPC calls."""
        # ... 100+ lines of existing code ...


# How tests would use it:
def test_curve_pool_with_bot_data_override():
    """Test using Bot with data override - no mocking needed."""

    # Setup test data
    dai = Erc20Token(
        address="0x6B175474E89094C44Da98b954EedeAC495271d0F",
        name="DAI",
        symbol="DAI",
        decimals=18,
    )
    usdc = Erc20Token(
        address="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        name="USD Coin",
        symbol="USDC",
        decimals=6,
    )

    test_data = CurvePoolTestData(
        tokens=(dai, usdc),
        balances=(10**20, 10**20),
        a_coefficient=2000,
        fee=4000000,
        admin_fee=5000000000,
        # Optional: override fetchers for specific test scenarios
        timestamp_fetcher=lambda block: 1700000000,
    )

    # Create Bot with test data
    bot = BotWithDataOverride(config={})
    pool = bot.build_curve_pool(
        "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7",
        test_data=test_data,  # ← Data override, no RPC
    )

    # Test pool logic
    assert pool.a_coefficient == 2000
    result = pool.get_dy(0, 1, 10**18, block_identifier=18_000_000)
    assert result > 0


# =============================================================================
# PATTERN 2: Pluggable Data Source (Clean Architecture)
# =============================================================================


class DataSource(Protocol):
    """Protocol for fetching data needed to construct pools."""

    def get_curve_pool_data(
        self,
        address: str,
        block: int,
    ) -> CurvePoolTestData:
        """Fetch all data needed for a Curve pool."""
        ...

    def get_token_metadata(
        self,
        address: str,
    ) -> tuple[str, str, int]:  # name, symbol, decimals
        """Fetch token metadata."""
        ...


class OnChainDataSource:
    """Production data source - fetches from blockchain."""

    def __init__(self, web3):
        self.w3 = web3

    def get_curve_pool_data(self, address: str, block: int) -> CurvePoolTestData:
        """Fetch pool data via RPC calls."""
        # All the RPC calls currently in Bot.build_curve_pool()
        # - coins(), balances(), A(), fee(), admin_fee()
        # - lending token detection
        # - crypto pool parameter detection
        # - metapool detection
        # ... (move existing code here)

    def get_token_metadata(self, address: str) -> tuple[str, str, int]:
        """Fetch token metadata via RPC."""
        # name(), symbol(), decimals()


class FakeDataSource:
    """Test data source - returns pre-configured data."""

    def __init__(self):
        self._pools: dict[str, CurvePoolTestData] = {}
        self._tokens: dict[str, tuple[str, str, int]] = {}

    def add_pool(self, address: str, data: CurvePoolTestData):
        """Register test data for a pool."""
        self._pools[address.lower()] = data

    def add_token(self, address: str, name: str, symbol: str, decimals: int):
        """Register test data for a token."""
        self._tokens[address.lower()] = (name, symbol, decimals)

    def get_curve_pool_data(self, address: str, block: int) -> CurvePoolTestData:
        return self._pools[address.lower()]

    def get_token_metadata(self, address: str) -> tuple[str, str, int]:
        return self._tokens[address.lower()]


class RecordingDataSource:
    """Wrapper that records all data fetches for replay."""

    def __init__(self, source: DataSource, output_path: str):
        self.source = source
        self.output_path = output_path
        self.recording: list = []

    def get_curve_pool_data(self, address: str, block: int) -> CurvePoolTestData:
        data = self.source.get_curve_pool_data(address, block)
        self.recording.append({
            "method": "get_curve_pool_data",
            "args": {"address": address, "block": block},
            "result": data,  # Would need serialization
        })
        return data

    def save(self):
        """Save recording to file."""

        with pathlib.Path(self.output_path).open("w", encoding="utf-8") as f:
            json.dump(self.recording, f, indent=2)


class ReplayingDataSource:
    """Replays recorded data fetches."""

    def __init__(self, recording_path: str):

        with pathlib.Path(recording_path).open(encoding="utf-8") as f:
            self.recording = json.load(f)
        self._index = 0

    def get_curve_pool_data(self, address: str, block: int) -> CurvePoolTestData:
        entry = self.recording[self._index]
        self._index += 1
        # Would need deserialization
        return entry["result"]


# How Bot would use DataSource:
class BotWithDataSource:
    """Refactored Bot that delegates I/O to a data source."""

    def __init__(self, config, data_source: DataSource | None = None):
        self.config = config
        self.data_source = data_source or OnChainDataSource(...)
        self.pools = {}
        self.tokens = {}

    def build_curve_pool(self, address: str, block: int | None = None):
        """Build pool using configured data source."""

        # Fetch data via data source (could be on-chain, fake, or replayed)
        data = self.data_source.get_curve_pool_data(address, block or 18_000_000)

        # Construct pool (pure factory, no I/O)
        pool = CurveStableswapPool(
            address=address,
            tokens=data.tokens,
            a_coefficient=data.a_coefficient,
            fee=data.fee,
            admin_fee=data.admin_fee,
            balances=data.balances,
            state_block=data.state_block,
            **data.get_fetchers(),
        )

        # Register in bot's pool registry
        self.pools[address] = pool
        return pool


# How tests would use it:
def test_curve_pool_with_fake_data_source():
    """Test using Bot with FakeDataSource - no mocking needed."""

    # Setup fake data source
    fake_source = FakeDataSource()

    dai = Erc20Token(address="0x...", name="DAI", symbol="DAI", decimals=18)
    usdc = Erc20Token(address="0x...", name="USDC", symbol="USDC", decimals=6)

    fake_source.add_pool(
        "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7",
        CurvePoolTestData(
            tokens=(dai, usdc),
            balances=(10**20, 10**20),
            a_coefficient=2000,
            fee=4000000,
            admin_fee=5000000000,
        ),
    )

    # Create Bot with fake data source
    bot = BotWithDataSource(config={}, data_source=fake_source)

    # Use same API as production
    pool = bot.build_curve_pool("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")

    # Test pool
    assert pool.a_coefficient == 2000


def test_curve_pool_with_recording():
    """Test using Bot with recorded production data."""

    # Replayer loads recorded RPC calls
    replayer = ReplayingDataSource("tests/fixtures/curve_tripool_recording.json")

    # Bot uses replayer instead of real RPC
    bot = BotWithDataSource(config={}, data_source=replayer)

    # Same API as production, but replays recorded data
    pool = bot.build_curve_pool("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")

    # Test against real production data
    assert pool.a_coefficient == 2000  # Real value from mainnet


# =============================================================================
# COMPARISON
# =============================================================================

"""
Pattern 1 (Data Override):
  - Quick to implement (add test_data parameter)
  - Tests must provide all data
  - Good for: Simple unit tests

Pattern 2 (Pluggable Data Source):
  - More refactoring, but cleaner architecture
  - Same Bot API works everywhere
  - Enables: Recording, caching, backtesting, simulations

Recommendation: Start with Pattern 1, refactor to Pattern 2
"""


if __name__ == "__main__":
    # Quick demonstration
    print("Pattern 1: Data Override")
    print("  Bot.build_pool(address, test_data={...})")
    print()
    print("Pattern 2: Pluggable Data Source")
    print("  Bot(config, data_source=FakeDataSource(...))")
    print("  Bot(config, data_source=OnChainDataSource(...))")
    print("  Bot(config, data_source=ReplayingDataSource(...))")
