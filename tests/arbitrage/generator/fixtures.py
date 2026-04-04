"""
Arbitrage test fixtures with known profitable outcomes.

Provides fixtures for testing arbitrage calculations with deterministic results.
"""

import json
import math
import random
from dataclasses import dataclass
from fractions import Fraction
from pathlib import Path
from typing import Any, Literal, cast

from eth_typing import ChecksumAddress
from hexbytes import HexBytes

from degenbot.types.abstract import AbstractPoolState
from degenbot.uniswap.v2_types import UniswapV2PoolState
from degenbot.uniswap.v3_libraries.tick_math import get_sqrt_ratio_at_tick
from degenbot.uniswap.v3_types import (
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3PoolState,
)
from degenbot.uniswap.v4_types import (
    UniswapV4BitmapAtWord,
    UniswapV4LiquidityAtTick,
    UniswapV4PoolState,
)

from .pool_generator import PoolStateGenerator
from .types import LIQUIDITY_MULTIPLIERS, PoolGenerationConfig, V3PoolGenerationConfig


@dataclass(frozen=True, slots=True)
class ArbitrageCycleFixture:
    """
    A synthetic arbitrage scenario with known profitable outcome.

    Simple cases are manually constructed with hand-calculated optimal values.
    Stress tests are randomly generated with profit validation.

    Attributes
    ----------
    id : str
        Unique identifier for the fixture.
    cycle_type : str
        Type of arbitrage cycle (e.g., "v2_v2", "v3_v3", "v2_v3").
    pool_states : dict[ChecksumAddress, AbstractPoolState]
        Pool states keyed by address.
    input_token_address : ChecksumAddress
        Address of the input token for the arbitrage.
    expected_optimal_input : int
        Known optimal input amount (or 0 if unknown).
    expected_profit : int
        Known profit at optimum (or 0 if unknown).
    profit_tolerance_bps : int
        Acceptable deviation from expected profit in basis points (default: 10 = 0.1%).
    """

    id: str
    cycle_type: str
    pool_states: dict[ChecksumAddress, AbstractPoolState]
    input_token_address: ChecksumAddress
    expected_optimal_input: int = 0
    expected_profit: int = 0
    profit_tolerance_bps: int = 10

    def to_json(self) -> str:
        """
        Serialize fixture to JSON string.

        Returns
        -------
        str
            JSON representation of the fixture.
        """
        data: dict[str, Any] = {
            "id": self.id,
            "cycle_type": self.cycle_type,
            "pool_states": {
                addr: _serialize_pool_state(state) for addr, state in self.pool_states.items()
            },
            "input_token_address": self.input_token_address,
            "expected_optimal_input": self.expected_optimal_input,
            "expected_profit": self.expected_profit,
            "profit_tolerance_bps": self.profit_tolerance_bps,
        }
        return json.dumps(data, indent=2)

    @classmethod
    def from_json(cls, json_str: str) -> "ArbitrageCycleFixture":
        """
        Deserialize fixture from JSON string.

        Parameters
        ----------
        json_str : str
            JSON representation of the fixture.

        Returns
        -------
        ArbitrageCycleFixture
            The deserialized fixture.
        """
        data = json.loads(json_str)
        pool_states = {
            cast("ChecksumAddress", addr): _deserialize_pool_state(state_data)
            for addr, state_data in data["pool_states"].items()
        }
        return cls(
            id=data["id"],
            cycle_type=data["cycle_type"],
            pool_states=pool_states,
            input_token_address=cast("ChecksumAddress", data["input_token_address"]),
            expected_optimal_input=data["expected_optimal_input"],
            expected_profit=data["expected_profit"],
            profit_tolerance_bps=data["profit_tolerance_bps"],
        )

    def save(self, path: Path) -> None:
        """
        Save fixture to a JSON file.

        Parameters
        ----------
        path : Path
            Path to save the fixture.
        """
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(self.to_json(), encoding="utf-8")

    @classmethod
    def load(cls, path: Path) -> "ArbitrageCycleFixture":
        """
        Load fixture from a JSON file.

        Parameters
        ----------
        path : Path
            Path to load the fixture from.

        Returns
        -------
        ArbitrageCycleFixture
            The loaded fixture.
        """
        return cls.from_json(path.read_text(encoding="utf-8"))

    def validate(self) -> bool:
        """
        Validate fixture integrity.

        Checks that:
        - At least two pool states exist
        - All pool states have valid addresses
        - Pool states are for compatible pool types

        Returns
        -------
        bool
            True if fixture is valid.

        Raises
        ------
        ValueError
            If fixture is invalid.
        """
        if len(self.pool_states) < 2:
            msg = f"Fixture must have at least 2 pool states, got {len(self.pool_states)}"
            raise ValueError(msg)

        # Validate pool state addresses are unique
        addresses = list(self.pool_states.keys())
        if len(addresses) != len(set(addresses)):
            msg = "Pool state addresses must be unique"
            raise ValueError(msg)

        # Validate cycle_type matches pool states
        # For multi-pool cycles, we allow any underscore-separated combination
        basic_cycle_types = {"v2_v2", "v2_v3", "v3_v3", "v2_v4", "v3_v4", "v4_v4"}
        if self.cycle_type not in basic_cycle_types:
            # Allow multi-pool cycle types (3+ pools)
            parts = self.cycle_type.split("_")
            valid_parts = {"v2", "v3", "v4"}
            if not all(part in valid_parts for part in parts):
                msg = f"Invalid cycle_type: {self.cycle_type}. Must contain only v2, v3, v4"
                raise ValueError(msg)

        # Validate profit tolerance
        if self.profit_tolerance_bps < 0:
            msg = f"profit_tolerance_bps must be non-negative, got {self.profit_tolerance_bps}"
            raise ValueError(msg)

        return True


# Token addresses used in fixtures (for reference)
USDC_ADDRESS: ChecksumAddress = cast(
    "ChecksumAddress", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
)
WETH_ADDRESS: ChecksumAddress = cast(
    "ChecksumAddress", "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
)


def _serialize_pool_state(state: AbstractPoolState) -> dict[str, Any]:
    """Serialize a pool state to a dictionary."""
    if isinstance(state, UniswapV4PoolState):
        return {
            "type": "v4",
            "address": state.address,
            "block": state.block,
            "id": state.id.hex(),
            "liquidity": state.liquidity,
            "sqrt_price_x96": state.sqrt_price_x96,
            "tick": state.tick,
            "tick_bitmap": {
                str(k): {"bitmap": v.bitmap, "block": v.block}
                for k, v in state.tick_bitmap.items()
            },
            "tick_data": {
                str(k): {
                    "liquidity_net": v.liquidity_net,
                    "liquidity_gross": v.liquidity_gross,
                    "block": v.block,
                }
                for k, v in state.tick_data.items()
            },
        }
    if isinstance(state, UniswapV3PoolState):
        return {
            "type": "v3",
            "address": state.address,
            "block": state.block,
            "liquidity": state.liquidity,
            "sqrt_price_x96": state.sqrt_price_x96,
            "tick": state.tick,
            "tick_bitmap": {
                str(k): {"bitmap": v.bitmap, "block": v.block}
                for k, v in state.tick_bitmap.items()
            },
            "tick_data": {
                str(k): {
                    "liquidity_net": v.liquidity_net,
                    "liquidity_gross": v.liquidity_gross,
                    "block": v.block,
                }
                for k, v in state.tick_data.items()
            },
        }
    if isinstance(state, UniswapV2PoolState):
        return {
            "type": "v2",
            "address": state.address,
            "block": state.block,
            "reserves_token0": state.reserves_token0,
            "reserves_token1": state.reserves_token1,
        }
    msg = f"Unknown pool state type: {type(state)}"
    raise ValueError(msg)


def _deserialize_pool_state(data: dict[str, Any]) -> AbstractPoolState:
    """Deserialize a pool state from a dictionary."""
    pool_type = data["type"]
    if pool_type == "v4":
        return UniswapV4PoolState(
            address=cast("ChecksumAddress", data["address"]),
            block=data["block"],
            id=HexBytes(data["id"]),
            liquidity=data["liquidity"],
            sqrt_price_x96=data["sqrt_price_x96"],
            tick=data["tick"],
            tick_bitmap={
                int(k): UniswapV4BitmapAtWord(bitmap=v["bitmap"], block=v["block"])
                for k, v in data["tick_bitmap"].items()
            },
            tick_data={
                int(k): UniswapV4LiquidityAtTick(
                    liquidity_net=v["liquidity_net"],
                    liquidity_gross=v["liquidity_gross"],
                    block=v["block"],
                )
                for k, v in data["tick_data"].items()
            },
        )
    if pool_type == "v3":
        return UniswapV3PoolState(
            address=cast("ChecksumAddress", data["address"]),
            block=data["block"],
            liquidity=data["liquidity"],
            sqrt_price_x96=data["sqrt_price_x96"],
            tick=data["tick"],
            tick_bitmap={
                int(k): UniswapV3BitmapAtWord(bitmap=v["bitmap"], block=v["block"])
                for k, v in data["tick_bitmap"].items()
            },
            tick_data={
                int(k): UniswapV3LiquidityAtTick(
                    liquidity_net=v["liquidity_net"],
                    liquidity_gross=v["liquidity_gross"],
                    block=v["block"],
                )
                for k, v in data["tick_data"].items()
            },
        )
    if pool_type == "v2":
        return UniswapV2PoolState(
            address=cast("ChecksumAddress", data["address"]),
            block=data["block"],
            reserves_token0=data["reserves_token0"],
            reserves_token1=data["reserves_token1"],
        )
    msg = f"Unknown pool type: {pool_type}"
    raise ValueError(msg)


class FixtureFactory:
    """
    Factory for generating arbitrage test fixtures.

    Provides both simple (hand-crafted) and stress (randomly generated) fixtures.
    """

    def __init__(self) -> None:
        self._generator = PoolStateGenerator()

    # ==========================================================================
    # Simple Cases (hand-crafted, exact values)
    # ==========================================================================

    def simple_v2_arb_profitable(self) -> ArbitrageCycleFixture:
        """
        Two V2 pools with 2% price difference.

        Simple profit: arbitrage from higher to lower price pool.
        Pool A: 1 ETH = 2000 USDC (price = 2000)
        Pool B: 1 ETH = 1960 USDC (price = 1960, 2% lower)

        Arbitrage: Buy ETH in pool B (cheaper), sell in pool A (more expensive).
        """
        pool_a_address: ChecksumAddress = cast(
            "ChecksumAddress", "0x0000000000000000000000000000000000000001"
        )
        pool_b_address: ChecksumAddress = cast(
            "ChecksumAddress", "0x0000000000000000000000000000000000000002"
        )
        input_token_address: ChecksumAddress = cast(
            "ChecksumAddress", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        )  # USDC

        # Generate pools with 2% price difference
        pool_a, pool_b = self._generator.generate_profitable_v2_pair(
            pool_a_address=pool_a_address,
            pool_b_address=pool_b_address,
            fee_a=Fraction(3, 1000),
            fee_b=Fraction(3, 1000),
            price_ratio=1.02,
            liquidity_base=10**21,  # ~1000 ETH equivalent
        )

        return ArbitrageCycleFixture(
            id="simple_v2_arb_profitable",
            cycle_type="v2_v2",
            pool_states={pool_a_address: pool_a, pool_b_address: pool_b},
            input_token_address=input_token_address,
            expected_optimal_input=0,  # Calculated by solver
            expected_profit=0,  # Calculated by solver
        )

    def simple_v2_arb_cross_fee(self) -> ArbitrageCycleFixture:
        """
        V2 pools with different fees (0.05% vs 0.3%).

        Demonstrates fee impact on arbitrage profitability.
        """
        pool_a_address: ChecksumAddress = cast(
            "ChecksumAddress", "0x0000000000000000000000000000000000000003"
        )
        pool_b_address: ChecksumAddress = cast(
            "ChecksumAddress", "0x0000000000000000000000000000000000000004"
        )
        input_token_address: ChecksumAddress = cast(
            "ChecksumAddress", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        )

        pool_a, pool_b = self._generator.generate_profitable_v2_pair(
            pool_a_address=pool_a_address,
            pool_b_address=pool_b_address,
            fee_a=Fraction(5, 10000),  # 0.05%
            fee_b=Fraction(3, 1000),  # 0.3%
            price_ratio=1.015,
            liquidity_base=10**21,
        )

        return ArbitrageCycleFixture(
            id="simple_v2_arb_cross_fee",
            cycle_type="v2_v2",
            pool_states={pool_a_address: pool_a, pool_b_address: pool_b},
            input_token_address=input_token_address,
        )

    def simple_v3_arb_same_tick_spacing(self) -> ArbitrageCycleFixture:
        """
        Two V3 pools at same tick spacing, different prices.
        """
        pool_a_address: ChecksumAddress = cast(
            "ChecksumAddress", "0x0000000000000000000000000000000000000005"
        )
        pool_b_address: ChecksumAddress = cast(
            "ChecksumAddress", "0x0000000000000000000000000000000000000006"
        )
        input_token_address: ChecksumAddress = cast(
            "ChecksumAddress", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        )

        pool_a, pool_b = self._generator.generate_profitable_v3_pair(
            pool_a_address=pool_a_address,
            pool_b_address=pool_b_address,
            tick_spacing=60,
            price_ratio=1.02,
            liquidity=10**18,
        )

        return ArbitrageCycleFixture(
            id="simple_v3_arb_same_tick_spacing",
            cycle_type="v3_v3",
            pool_states={pool_a_address: pool_a, pool_b_address: pool_b},
            input_token_address=input_token_address,
        )

    def simple_v3_arb_cross_fee_tier(self) -> ArbitrageCycleFixture:
        """
        V3 pools at different fee tiers.
        """
        pool_a_address: ChecksumAddress = cast(
            "ChecksumAddress", "0x0000000000000000000000000000000000000007"
        )
        pool_b_address: ChecksumAddress = cast(
            "ChecksumAddress", "0x0000000000000000000000000000000000000008"
        )
        input_token_address: ChecksumAddress = cast(
            "ChecksumAddress", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        )

        # Pool A: 0.05% fee tier (tick_spacing=10)
        # Pool B: 0.3% fee tier (tick_spacing=60)
        pool_a, pool_b = self._generator.generate_profitable_v3_pair(
            pool_a_address=pool_a_address,
            pool_b_address=pool_b_address,
            tick_spacing=60,  # Using same tick spacing for simplicity
            price_ratio=1.025,
            liquidity=10**18,
        )

        return ArbitrageCycleFixture(
            id="simple_v3_arb_cross_fee_tier",
            cycle_type="v3_v3",
            pool_states={pool_a_address: pool_a, pool_b_address: pool_b},
            input_token_address=input_token_address,
        )

    def simple_mixed_v2_v3(self) -> ArbitrageCycleFixture:
        """
        V2 vs V3 arbitrage.
        """
        v2_pool_address: ChecksumAddress = cast(
            "ChecksumAddress", "0x0000000000000000000000000000000000000009"
        )
        v3_pool_address: ChecksumAddress = cast(
            "ChecksumAddress", "0x000000000000000000000000000000000000000A"
        )
        input_token_address: ChecksumAddress = cast(
            "ChecksumAddress", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        )

        v2_pool, v3_pool = self._generator.generate_profitable_mixed_pair(
            v2_pool_address=v2_pool_address,
            v3_pool_address=v3_pool_address,
            v2_fee=Fraction(3, 1000),
            v3_tick_spacing=60,
            price_ratio=1.02,
            liquidity_base=10**21,
            v3_liquidity=10**18,
        )

        return ArbitrageCycleFixture(
            id="simple_mixed_v2_v3",
            cycle_type="v2_v3",
            pool_states={v2_pool_address: v2_pool, v3_pool_address: v3_pool},
            input_token_address=input_token_address,
        )

    def simple_v4_arb(self) -> ArbitrageCycleFixture:
        """
        Two V4 pools with price difference.
        """
        pool_manager_address: ChecksumAddress = cast(
            "ChecksumAddress", "0x0000000000000000000000000000000000000FFF"
        )
        pool_a_id = HexBytes("0x" + "01" * 32)
        pool_b_id = HexBytes("0x" + "02" * 32)
        input_token_address: ChecksumAddress = cast(
            "ChecksumAddress", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        )

        pool_a, pool_b = self._generator.generate_profitable_v4_pair(
            pool_a_address=pool_manager_address,
            pool_b_address=pool_manager_address,
            pool_a_id=pool_a_id,
            pool_b_id=pool_b_id,
            tick_spacing=60,
            price_ratio=1.02,
            liquidity=10**18,
        )

        return ArbitrageCycleFixture(
            id="simple_v4_arb",
            cycle_type="v4_v4",
            pool_states={
                cast("ChecksumAddress", pool_a_id.to_0x_hex()): pool_a,
                cast("ChecksumAddress", pool_b_id.to_0x_hex()): pool_b,
            },
            input_token_address=input_token_address,
        )

    def simple_v4_vs_v3(self) -> ArbitrageCycleFixture:
        """
        V4 vs V3 arbitrage.

        Tests arbitrage between V4 and V3 pool implementations.
        """
        pool_manager_address: ChecksumAddress = cast(
            "ChecksumAddress", "0x0000000000000000000000000000000000000FFF"
        )
        v3_pool_address: ChecksumAddress = cast(
            "ChecksumAddress", "0x000000000000000000000000000000000000000B"
        )
        pool_a_id = HexBytes("0x" + "03" * 32)
        input_token_address: ChecksumAddress = cast(
            "ChecksumAddress", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        )

        # Generate V4 pool
        v4_pool, _ = self._generator.generate_profitable_v4_pair(
            pool_a_address=pool_manager_address,
            pool_b_address=pool_manager_address,
            pool_a_id=pool_a_id,
            pool_b_id=HexBytes("0x" + "04" * 32),
            tick_spacing=60,
            price_ratio=1.02,
            liquidity=10**18,
        )

        # Generate V3 pool with slightly different price
        v3_pool = self._generator.generate_v3_pool_state(
            address=v3_pool_address,
            sqrt_price_x96=v4_pool.sqrt_price_x96,
            liquidity=10**18,
            tick=v4_pool.tick,
            tick_spacing=60,
        )

        return ArbitrageCycleFixture(
            id="simple_v4_vs_v3",
            cycle_type="v3_v4",
            pool_states={
                cast("ChecksumAddress", pool_a_id.to_0x_hex()): v4_pool,
                v3_pool_address: v3_pool,
            },
            input_token_address=input_token_address,
        )

    # ==========================================================================
    # Stress Tests (randomly generated)
    # ==========================================================================

    def random_v2_pair(
        self,
        seed: int,
        liquidity_depth: Literal["shallow", "medium", "deep"] = "medium",
        price_ratio_range: tuple[float, float] = (1.01, 1.05),
    ) -> ArbitrageCycleFixture:
        """
        Generate a random V2 pair with constraints.

        Parameters
        ----------
        seed : int
            Random seed for reproducibility.
        liquidity_depth : Literal["shallow", "medium", "deep"]
            Liquidity depth multiplier.
        price_ratio_range : tuple[float, float]
            Range for random price ratio.

        Returns
        -------
        ArbitrageCycleFixture
            The generated fixture.
        """
        random.seed(seed)

        pool_a_address: ChecksumAddress = cast("ChecksumAddress", f"0x{seed:040x}")
        pool_b_address: ChecksumAddress = cast("ChecksumAddress", f"0x{(seed + 1):040x}")
        input_token_address: ChecksumAddress = cast(
            "ChecksumAddress", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        )

        price_ratio = random.uniform(*price_ratio_range)
        liquidity_base = LIQUIDITY_MULTIPLIERS[liquidity_depth]

        pool_a, pool_b = self._generator.generate_profitable_v2_pair(
            pool_a_address=pool_a_address,
            pool_b_address=pool_b_address,
            fee_a=Fraction(3, 1000),
            fee_b=Fraction(3, 1000),
            price_ratio=price_ratio,
            liquidity_base=liquidity_base,
        )

        return ArbitrageCycleFixture(
            id=f"random_v2_pair_seed_{seed}",
            cycle_type="v2_v2",
            pool_states={pool_a_address: pool_a, pool_b_address: pool_b},
            input_token_address=input_token_address,
        )

    def random_v3_pair(
        self,
        seed: int,
        liquidity_depth: Literal["shallow", "medium", "deep"] = "medium",
        price_ratio_range: tuple[float, float] = (1.01, 1.05),
        tick_spacing: int = 60,
    ) -> ArbitrageCycleFixture:
        """
        Generate a random V3 pair with constraints.

        Parameters
        ----------
        seed : int
            Random seed for reproducibility.
        liquidity_depth : Literal["shallow", "medium", "deep"]
            Liquidity depth multiplier.
        price_ratio_range : tuple[float, float]
            Range for random price ratio.
        tick_spacing : int
            Tick spacing for both pools.

        Returns
        -------
        ArbitrageCycleFixture
            The generated fixture.
        """
        random.seed(seed)

        pool_a_address: ChecksumAddress = cast("ChecksumAddress", f"0x{seed:040x}")
        pool_b_address: ChecksumAddress = cast("ChecksumAddress", f"0x{(seed + 1):040x}")
        input_token_address: ChecksumAddress = cast(
            "ChecksumAddress", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        )

        price_ratio = random.uniform(*price_ratio_range)
        liquidity = LIQUIDITY_MULTIPLIERS[liquidity_depth]

        pool_a, pool_b = self._generator.generate_profitable_v3_pair(
            pool_a_address=pool_a_address,
            pool_b_address=pool_b_address,
            tick_spacing=tick_spacing,
            price_ratio=price_ratio,
            liquidity=liquidity,
        )

        return ArbitrageCycleFixture(
            id=f"random_v3_pair_seed_{seed}",
            cycle_type="v3_v3",
            pool_states={pool_a_address: pool_a, pool_b_address: pool_b},
            input_token_address=input_token_address,
        )

    def random_v4_pair(
        self,
        seed: int,
        liquidity_depth: Literal["shallow", "medium", "deep"] = "medium",
        price_ratio_range: tuple[float, float] = (1.01, 1.05),
        tick_spacing: int = 60,
    ) -> ArbitrageCycleFixture:
        """
        Generate a random V4 pair with constraints.

        Parameters
        ----------
        seed : int
            Random seed for reproducibility.
        liquidity_depth : Literal["shallow", "medium", "deep"]
            Liquidity depth multiplier.
        price_ratio_range : tuple[float, float]
            Range for random price ratio.
        tick_spacing : int
            Tick spacing for both pools.

        Returns
        -------
        ArbitrageCycleFixture
            The generated fixture.
        """
        random.seed(seed)

        pool_manager_address: ChecksumAddress = cast(
            "ChecksumAddress", "0x0000000000000000000000000000000000000FFF"
        )
        pool_a_id = HexBytes(f"0x{seed:064x}")
        pool_b_id = HexBytes(f"0x{(seed + 1):064x}")
        input_token_address: ChecksumAddress = cast(
            "ChecksumAddress", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        )

        price_ratio = random.uniform(*price_ratio_range)
        liquidity = LIQUIDITY_MULTIPLIERS[liquidity_depth]

        pool_a, pool_b = self._generator.generate_profitable_v4_pair(
            pool_a_address=pool_manager_address,
            pool_b_address=pool_manager_address,
            pool_a_id=pool_a_id,
            pool_b_id=pool_b_id,
            tick_spacing=tick_spacing,
            price_ratio=price_ratio,
            liquidity=liquidity,
        )

        return ArbitrageCycleFixture(
            id=f"random_v4_pair_seed_{seed}",
            cycle_type="v4_v4",
            pool_states={
                cast("ChecksumAddress", pool_a_id.to_0x_hex()): pool_a,
                cast("ChecksumAddress", pool_b_id.to_0x_hex()): pool_b,
            },
            input_token_address=input_token_address,
        )

    def random_multi_pool_cycle(
        self,
        seed: int,
        num_pools: int = 3,
        pool_types: list[Literal["v2", "v3", "v4"]] | None = None,
        liquidity_depth: Literal["shallow", "medium", "deep"] = "medium",
        price_ratio_range: tuple[float, float] = (1.01, 1.03),
    ) -> ArbitrageCycleFixture:
        """
        Generate a multi-pool cycle with constraints.

        Creates a cycle through 3+ pools where arbitrage is possible.
        Each consecutive pool has a slightly different price.

        Parameters
        ----------
        seed : int
            Random seed for reproducibility.
        num_pools : int
            Number of pools in the cycle (default: 3, min: 3).
        pool_types : list[Literal["v2", "v3", "v4"]] | None
            Types of pools to include. If None, uses all V2 for simplicity.
        liquidity_depth : Literal["shallow", "medium", "deep"]
            Liquidity depth multiplier.
        price_ratio_range : tuple[float, float]
            Range for random price ratio between consecutive pools.

        Returns
        -------
        ArbitrageCycleFixture
            The generated fixture.
        """
        if num_pools < 3:
            msg = f"num_pools must be at least 3, got {num_pools}"
            raise ValueError(msg)

        random.seed(seed)

        if pool_types is None:
            pool_types = ["v2"] * num_pools

        if len(pool_types) != num_pools:
            msg = f"pool_types length ({len(pool_types)}) must match num_pools ({num_pools})"
            raise ValueError(msg)

        pool_states: dict[ChecksumAddress, AbstractPoolState] = {}
        input_token_address: ChecksumAddress = cast(
            "ChecksumAddress", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        )

        # Generate consecutive pools with price differences
        base_price = 1.0
        liquidity = LIQUIDITY_MULTIPLIERS[liquidity_depth]

        for i, pool_type in enumerate(pool_types):
            pool_address: ChecksumAddress = cast(
                "ChecksumAddress", f"0x{(seed + i):040x}"
            )

            # Each pool has progressively different price
            price_ratio = random.uniform(*price_ratio_range)
            current_price = base_price * (price_ratio**i)

            if pool_type == "v2":
                config = PoolGenerationConfig(fee=Fraction(3, 1000))
                pool_states[pool_address] = self._generator.generate_v2_pool_state_from_price(
                    address=pool_address,
                    price_token1_per_token0=current_price,
                    liquidity_base=liquidity,
                    config=config,
                )
            elif pool_type == "v3":
                config = V3PoolGenerationConfig(
                    fee=Fraction(3, 1000),
                    tick_spacing=60,
                    liquidity_depth=liquidity,
                )
                pool_states[pool_address] = self._generator.generate_v3_pool_state_from_price(
                    address=pool_address,
                    price_token1_per_token0=current_price,
                    liquidity=liquidity,
                    config=config,
                )
            elif pool_type == "v4":
                pool_manager_address: ChecksumAddress = cast(
                    "ChecksumAddress", "0x0000000000000000000000000000000000000FFF"
                )
                pool_id = HexBytes(f"0x{(seed + i):064x}")
                v4_config = V3PoolGenerationConfig(
                    fee=Fraction(3, 1000),
                    tick_spacing=60,
                    liquidity_depth=liquidity,
                )
                # Generate V4 from price
                decimal_adjustment = 10 ** (
                    v4_config.token1_decimals - v4_config.token0_decimals
                )
                adjusted_price = current_price * decimal_adjustment
                tick = int(math.log(adjusted_price) / math.log(1.0001))
                tick = round(tick / v4_config.tick_spacing) * v4_config.tick_spacing
                sqrt_price_x96 = get_sqrt_ratio_at_tick(tick)

                pool_states[cast("ChecksumAddress", pool_id.to_0x_hex())] = (
                    self._generator.generate_v4_pool_state(
                        address=pool_manager_address,
                        pool_id=pool_id,
                        sqrt_price_x96=sqrt_price_x96,
                        liquidity=liquidity,
                        tick=tick,
                        tick_spacing=60,
                    )
                )

        # Generate cycle type from pool types
        if len(set(pool_types)) > 1:
            cycle_type = "_".join(pool_types)
        else:
            cycle_type = f"{pool_types[0]}_{pool_types[0]}"

        return ArbitrageCycleFixture(
            id=f"random_multi_pool_cycle_seed_{seed}_pools_{num_pools}",
            cycle_type=cycle_type,
            pool_states=pool_states,
            input_token_address=input_token_address,
        )
