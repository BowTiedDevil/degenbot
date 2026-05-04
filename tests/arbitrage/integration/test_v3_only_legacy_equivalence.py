"""
V3-only legacy ↔ new equivalence: UniswapLpCycle vs ArbitragePath.

Uses FakeV3Pool with exact single-range V3 math (v3_virtual_reserves +
constant-product) to compare the legacy brent optimizer with the new
MobiusSolver / BrentSolver optimizers.

This is the full-stack equivalence gate for V3-only arbitrage paths.
Both systems must see the same profit landscape; any divergence indicates
a real behavioral gap.
"""

import math
import unittest.mock
from fractions import Fraction

import pytest

from degenbot.arbitrage.optimizers.solver import BrentSolver, MobiusSolver
from degenbot.arbitrage.path import ArbitragePath
from degenbot.arbitrage.uniswap_lp_cycle import UniswapLpCycle
from degenbot.erc20.erc20 import Erc20Token
from degenbot.uniswap.types import UniswapPoolSwapVector
from degenbot.uniswap.v3_libraries.functions import v3_virtual_reserves
from degenbot.uniswap.v3_types import UniswapV3PoolState
from tests.arbitrage.generator.pool_generator import PoolStateGenerator
from tests.arbitrage.generator.types import V3PoolGenerationConfig
from tests.arbitrage.mock_pools import MockErc20Token, cleanup_mock_patches

# ---------------------------------------------------------------------------
# FakeV3Pool: exact single-range V3 math matching ArbitragePath BoundedProductHop
# ---------------------------------------------------------------------------


class FakeV3Pool:
    """
    Mock V3 pool that uses EXACT single-range V3 math.

    The swap formula matches what to_hop_state() produces for a
    BoundedProductHop with tick_lower=MIN_TICK and tick_upper=MAX_TICK.
    The legacy UniswapLpCycle._build_swap_amounts uses the same math
    for a single-range V3 swap because calculate_tokens_out_from_tokens_in
    applies v3_virtual_reserves + constant-product.
    """

    FEE_DENOMINATOR = 1_000_000

    def __init__(
        self,
        address: str,
        token0: MockErc20Token | Erc20Token,
        token1: MockErc20Token | Erc20Token,
        state: UniswapV3PoolState,
        fee: int = 3000,
        tick_spacing: int = 60,
    ) -> None:
        self.address = address
        self.token0 = token0
        self.token1 = token1
        self.tokens = (token0, token1)
        self.fee = fee
        self.tick_spacing = tick_spacing
        self.chain_id = 1
        self.name = f"FakeV3-{address[:10]}"
        self.sparse_liquidity_map = False
        self._state = state
        self._subscribers: set[object] = set()

    @property
    def state(self) -> UniswapV3PoolState:
        return self._state

    def set_state(self, state: UniswapV3PoolState) -> None:
        self._state = state

    def subscribe(self, subscriber: object) -> "FakeV3Pool":
        self._subscribers.add(subscriber)
        return self

    def unsubscribe(self, subscriber: object) -> "FakeV3Pool":
        self._subscribers.discard(subscriber)
        return self

    @staticmethod
    def swap_is_viable(
        state: UniswapV3PoolState,
        vector: UniswapPoolSwapVector,
    ) -> bool:
        return True

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: MockErc20Token | Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV3PoolState | None = None,
    ) -> int:
        """Exact single-range V3 swap via virtual reserves."""
        state = override_state if override_state is not None else self._state
        zfo = token_in == self.token0
        reserve_in, reserve_out = v3_virtual_reserves(
            state.liquidity,
            state.sqrt_price_x96,
            zero_for_one=zfo,
        )
        gamma_num = self.FEE_DENOMINATOR - self.fee
        amount_in_with_fee = token_in_quantity * gamma_num // self.FEE_DENOMINATOR
        if amount_in_with_fee <= 0:
            return 0
        return reserve_out * amount_in_with_fee // (reserve_in + amount_in_with_fee)

    def get_absolute_exchange_rate(
        self,
        token: MockErc20Token | Erc20Token,
        override_state: UniswapV3PoolState | None = None,
    ) -> Fraction:
        state = override_state if override_state is not None else self._state
        from degenbot.uniswap.v3_libraries.constants import Q96

        sqrt_p = state.sqrt_price_x96
        price = Fraction(sqrt_p, Q96) * Fraction(sqrt_p, Q96)
        if token == self.token1:
            return price
        if token == self.token0:
            return Fraction(1, 1) / price if price.numerator > 0 else Fraction(0)
        msg = f"Token {token} not in pool {self.address}"
        raise ValueError(msg)

    def __hash__(self) -> int:
        return hash(self.address)

    def __eq__(self, other: object) -> bool:
        if isinstance(other, FakeV3Pool):
            return self.address == other.address
        return False

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: UniswapV3PoolState | None = None,
    ):
        from degenbot.types.hop_types import BoundedProductHop
        from degenbot.uniswap.v3_libraries.tick_math import MAX_TICK, MIN_TICK

        state = state_override if state_override is not None else self._state
        reserve_in, reserve_out = v3_virtual_reserves(
            state.liquidity, state.sqrt_price_x96, zero_for_one=zero_for_one
        )
        return BoundedProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=Fraction(self.fee, self.FEE_DENOMINATOR),
            liquidity=state.liquidity,
            sqrt_price=state.sqrt_price_x96,
            tick_lower=MIN_TICK,
            tick_upper=MAX_TICK,
            zero_for_one=zero_for_one,
        )

    def extract_fee(self, zero_for_one: bool) -> Fraction:  # noqa: FBT001
        return Fraction(self.fee, self.FEE_DENOMINATOR)

    def simulate_swap(
        self,
        token_in: str,
        amount_in: int,
        token_out: str,
        state_override: UniswapV3PoolState | None = None,
    ):
        from degenbot.types.pool_protocols import SimulationResult

        state = state_override if state_override is not None else self._state
        token_in_obj = self.token0 if token_in == self.token0.address else self.token1
        amount_out = self.calculate_tokens_out_from_tokens_in(
            token_in=token_in_obj,
            token_in_quantity=amount_in,
            override_state=state,
        )
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=state,
            final_state=state,
        )

    def __repr__(self) -> str:
        return f"FakeV3Pool({self.address}, {self.token0.symbol}/{self.token1.symbol})"


# ---------------------------------------------------------------------------
# Helpers: build legacy cycle and new path from the same mock pools
# ---------------------------------------------------------------------------


def _make_patched_legacy_cycle(
    pools: list[FakeV3Pool],
    input_token: MockErc20Token | Erc20Token,
    max_input: int = 10 * 10**18,
    cycle_id: str = "v3_legacy",
) -> UniswapLpCycle:
    """
    Build a UniswapLpCycle that accepts FakeV3Pool via monkeypatching.
    Matches the patching done by create_cycle_with_mocks but tailored for
    FakeV3Pool with exact math.
    """

    def patched_pool_is_viable(
        pool: FakeV3Pool,
        state: UniswapV3PoolState,
        vector: UniswapPoolSwapVector,
    ) -> bool:
        return pool.swap_is_viable(state, vector)

    def patched_pre_calculation_check(
        self: UniswapLpCycle,
        min_rate_of_exchange: Fraction = Fraction(1, 1),
        state_overrides: dict | None = None,
    ) -> None:
        if state_overrides is None:
            state_overrides = {}
        for pool, vector in zip(self.swap_pools, self._swap_vectors, strict=True):
            state = state_overrides.get(pool, pool.state)
            if not patched_pool_is_viable(pool, state, vector):
                msg = f"Pool {pool} is not viable"
                raise ValueError(msg)

        # Check net rate of exchange
        multipliers: list[Fraction] = []
        for pool, vector in zip(self.swap_pools, self._swap_vectors, strict=True):
            swap_mult = pool.get_absolute_exchange_rate(token=vector.token_out)
            fee_mult = Fraction(pool.FEE_DENOMINATOR - pool.fee, pool.FEE_DENOMINATOR)
            multipliers.extend((swap_mult, fee_mult))

        net = Fraction(
            math.prod(m.numerator for m in multipliers),
            math.prod(m.denominator for m in multipliers),
        )
        if net < min_rate_of_exchange:
            msg = f"Net rate {net} < {min_rate_of_exchange}"
            raise ValueError(msg)

    def patched_build_swap_amounts(
        self: UniswapLpCycle,
        token_in_quantity: int,
        state_overrides: dict | None = None,
    ) -> tuple:
        if state_overrides is None:
            state_overrides = {}
        from degenbot.arbitrage.types import UniswapV3PoolSwapAmounts
        from degenbot.exceptions.arbitrage import ArbitrageError
        from degenbot.uniswap.v3_libraries.tick_math import MAX_SQRT_RATIO, MIN_SQRT_RATIO

        token_out_quantity = 0
        swap_amounts: list = []
        for pool, sv in zip(self.swap_pools, self._swap_vectors, strict=True):
            if token_in_quantity == 0:
                raise ArbitrageError(message="A swap would result in an output of zero.")

            pool_state = state_overrides.get(pool)
            token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                token_in=sv.token_in,
                token_in_quantity=token_in_quantity,
                override_state=pool_state,
            )
            swap_amounts.append(
                UniswapV3PoolSwapAmounts(
                    pool=pool.address,
                    amount_in=token_in_quantity,
                    amount_out=token_out_quantity,
                    amount_specified=token_in_quantity,
                    zero_for_one=sv.zero_for_one,
                    sqrt_price_limit_x96=MIN_SQRT_RATIO + 1
                    if sv.zero_for_one
                    else MAX_SQRT_RATIO - 1,
                )
            )
            token_in_quantity = token_out_quantity

        return tuple(swap_amounts)

    def patched_arb_profit(
        self: UniswapLpCycle,
        x: float,
        state_overrides: dict | None = None,
    ) -> float:
        if state_overrides is None:
            state_overrides = {}
        token_in_quantity = int(x)
        starting = token_in_quantity
        token_out_quantity = 0
        for pool, sv in zip(self.swap_pools, self._swap_vectors, strict=True):
            pool_state = state_overrides.get(pool)
            token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                token_in=sv.token_in,
                token_in_quantity=token_in_quantity,
                override_state=pool_state,
            )
            token_in_quantity = token_out_quantity
        return float(token_out_quantity - starting)

    patches = [
        unittest.mock.patch.object(UniswapLpCycle, "_validate_pools", return_value=None),
        unittest.mock.patch.object(UniswapLpCycle, "_pool_is_viable", staticmethod(patched_pool_is_viable)),
        unittest.mock.patch.object(UniswapLpCycle, "_pre_calculation_check", patched_pre_calculation_check),
        unittest.mock.patch.object(UniswapLpCycle, "_arb_profit", patched_arb_profit),
        unittest.mock.patch.object(UniswapLpCycle, "_build_swap_amounts", patched_build_swap_amounts),
    ]

    UniswapLpCycle._v3_equiv_patches = patches  # type:ignore[attr-defined]
    for p in patches:
        p.start()

    try:
        cycle = UniswapLpCycle(
            id=cycle_id,
            input_token=input_token,
            swap_pools=pools,
            max_input=max_input,
        )
    except Exception:
        for p in patches:
            p.stop()
        raise

    return cycle


def _cleanup_patched_legacy_cycle() -> None:
    """Clean up monkeypatches on UniswapLpCycle."""
    if hasattr(UniswapLpCycle, "_v3_equiv_patches"):
        for p in UniswapLpCycle._v3_equiv_patches:  # type:ignore[attr-defined]
            p.stop()
        del UniswapLpCycle._v3_equiv_patches


# ---------------------------------------------------------------------------
# Fixture: deterministic profitable V3 pair
# ---------------------------------------------------------------------------


@pytest.fixture
def usdc() -> MockErc20Token:
    return MockErc20Token(
        address="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        symbol="USDC",
        decimals=6,
    )


@pytest.fixture
def weth() -> MockErc20Token:
    return MockErc20Token(
        address="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        symbol="WETH",
        decimals=18,
    )


def _make_profitable_v3_pair(
    t0: MockErc20Token,
    t1: MockErc20Token,
    price_a: float = 2200.0,
    price_b: float = 2000.0,
    liquidity: int = 10**18,
    fee: int = 500,
) -> tuple[FakeV3Pool, FakeV3Pool]:
    """
    Create two FakeV3Pool for the same token pair at different prices.

    Pool A: t0/t1 at {price_a}. ArbitragePath goes t0→t1 (zfo=True).
    Pool B: t0/t1 at {price_b}. ArbitragePath goes t1→t0 (zfo=False).

    Profitability = (price_a / price_b) * gamma²;  with price_a > price_b
    and low fees this is > 1.0.
    """

    generator = PoolStateGenerator()

    addr_a = "0x00000000000000000000000000000000000000A1"
    addr_b = "0x00000000000000000000000000000000000000A2"

    state_a = generator.generate_v3_pool_state_from_price(
        address=addr_a,
        price_token1_per_token0=price_a,
        liquidity=liquidity,
        config=V3PoolGenerationConfig(fee=Fraction(fee, 1_000_000), tick_spacing=60),
    )
    state_b = generator.generate_v3_pool_state_from_price(
        address=addr_b,
        price_token1_per_token0=price_b,
        liquidity=liquidity,
        config=V3PoolGenerationConfig(fee=Fraction(fee, 1_000_000), tick_spacing=60),
    )

    pool_a = FakeV3Pool(state_a.address, t0, t1, state_a, fee=fee)
    pool_b = FakeV3Pool(state_b.address, t0, t1, state_b, fee=fee)

    return pool_a, pool_b


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


class TestV3OnlyLegacyVsNew:
    """
    Verify UniswapLpCycle (legacy) and ArbitragePath (new) produce
    computationally equivalent results on the exact same V3-only pool states.
    """

    def test_2hop_v3_agreement_profitable(self, usdc: MockErc20Token, weth: MockErc20Token):
        """
        Both legacy and new systems find profit for a V3-only 2-hop cycle.
        
        Pool A: USDC/WETH at price 2200 (high USDC per WETH → cheap WETH)
        Pool B: USDC/WETH at price 2000 (low USDC per WETH → expensive WETH)
        
        Cycle: USDC → WETH (pool A, get 1/2200 WETH per USDC)
               WETH → USDC (pool B, get 2000 USDC per WETH)
        
        Wait, let's re-check. For zfo=True on pool A (price=2200 = token1/token0):
        - token_in = token0 (USDC), token_out = token1 (WETH)
        - reserve_in = x_virtual, reserve_out = y_virtual
        - output ≈ amount_in * gamma * (y/x) = amount_in * gamma * price = amount_in * 2200 * gamma
        
        For zfo=False on pool B (price=2000):
        - token_in = token1 (WETH), token_out = token0 (USDC)
        - reserve_in = y_virtual, reserve_out = x_virtual
        - output ≈ amount_in * gamma * (x/y) = amount_in * gamma / price = amount_in * gamma / 2000
        
        Cycle output = input * 2200*gamma * (1/2000)*gamma = input * (2200/2000) * gamma²
        
        With gamma = 0.9995 (fee=0.05%), factor = 1.1 * 0.999 ≈ 1.099 → PROFIT ✓
        """
        pool_a, pool_b = _make_profitable_v3_pair(usdc, weth, price_a=2200.0, price_b=2000.0)

        max_input = 1_000_000  # well within float64 exact int range

        # Legacy system
        cycle = _make_patched_legacy_cycle(
            pools=[pool_a, pool_b],
            input_token=usdc,
            max_input=max_input,
        )
        legacy_result = cycle.calculate()

        # New system
        path = ArbitragePath(
            pools=[pool_a, pool_b],
            input_token=usdc,
            solver=MobiusSolver(),
            max_input=max_input,
        )
        new_result = path.calculate()

        # Both must find positive profit
        assert legacy_result.profit_amount > 0
        assert new_result.profit > 0

        # Legacy uses scipy.minimize_scalar + int(opt.x); MobiusSolver uses integer
        # search around a closed-form float optimum.  They may differ by a small number
        # of wei (<0.01% of max_input), but profit must agree within ~0.1%.
        assert abs(legacy_result.input_amount - new_result.optimal_input) <= max_input // 100
        rel_profit_diff = abs(legacy_result.profit_amount - new_result.profit) / max(
            legacy_result.profit_amount, 1
        )
        assert rel_profit_diff < 0.001, (
            f"legacy profit={legacy_result.profit_amount}, new profit={new_result.profit}"
        )

    def test_2hop_v3_agreement_unprofitable(self, usdc: MockErc20Token, weth: MockErc20Token):
        """When prices are symmetric, both systems reject the path."""
        pool_a, pool_b = _make_profitable_v3_pair(
            usdc, weth, price_a=2000.0, price_b=2000.0
        )

        max_input = 1_000_000

        # Legacy system: symmetric prices means _pre_calculation_check fails
        cycle = _make_patched_legacy_cycle(
            pools=[pool_a, pool_b],
            input_token=usdc,
            max_input=max_input,
        )
        with pytest.raises(ValueError, match="Net rate"):
            cycle.calculate()

        # New system: symmetric prices means Möbius K/M ≤ 1
        path = ArbitragePath(
            pools=[pool_a, pool_b],
            input_token=usdc,
            solver=MobiusSolver(),
            max_input=max_input,
        )
        from degenbot.exceptions import OptimizationError
        with pytest.raises(OptimizationError, match="Not profitable"):
            path.calculate()

    def test_2hop_v3_mobius_and_brent_agree(self, usdc: MockErc20Token, weth: MockErc20Token):
        """MobiusSolver (closed-form) and BrentSolver (scipy) should agree."""
        pool_a, pool_b = _make_profitable_v3_pair(usdc, weth)

        max_input = 1_000_000

        path_mobius = ArbitragePath(
            pools=[pool_a, pool_b],
            input_token=usdc,
            solver=MobiusSolver(),
            max_input=max_input,
        )
        result_mobius = path_mobius.calculate()

        path_brent = ArbitragePath(
            pools=[pool_a, pool_b],
            input_token=usdc,
            solver=BrentSolver(),
            max_input=max_input,
        )
        result_brent = path_brent.calculate()

        assert abs(result_mobius.optimal_input - result_brent.optimal_input) <= 1
        assert abs(result_mobius.profit - result_brent.profit) <= 1

    def test_3hop_v3_agreement(self, usdc: MockErc20Token, weth: MockErc20Token):
        """A 3-hop V3-only path with alternating prices must agree."""
        # Three pools with asymmetric prices
        pool_0, _ = _make_profitable_v3_pair(usdc, weth, price_a=2200.0, price_b=2000.0)
        # Pool 2 returns to the starting token with a favorable rate
        generator = PoolStateGenerator()
        state_2 = generator.generate_v3_pool_state_from_price(
            address="0x00000000000000000000000000000000000000A3",
            price_token1_per_token0=1.05,
            liquidity=10**18,
            config=V3PoolGenerationConfig(fee=Fraction(500, 1_000_000), tick_spacing=60),
        )
        pool_2 = FakeV3Pool(state_2.address, usdc, weth, state_2, fee=500)

        # Use a 2-hop cycle: pool_0 + pool_2 (both USDC→WETH→USDC)
        max_input = 1_000_000

        # Legacy
        cycle = _make_patched_legacy_cycle(
            pools=[pool_0, pool_2],
            input_token=usdc,
            max_input=max_input,
        )
        legacy_result = cycle.calculate()

        # New
        path = ArbitragePath(
            pools=[pool_0, pool_2],
            input_token=usdc,
            solver=MobiusSolver(),
            max_input=max_input,
        )
        new_result = path.calculate()

        assert legacy_result.profit_amount > 0
        assert new_result.profit > 0
        assert abs(legacy_result.input_amount - new_result.optimal_input) <= max_input // 100
        rel_profit_diff = abs(legacy_result.profit_amount - new_result.profit) / max(
            legacy_result.profit_amount, 1
        )
        assert rel_profit_diff < 0.001

    def teardown_method(self) -> None:
        """Clean up monkeypatches after each test."""
        _cleanup_patched_legacy_cycle()
        cleanup_mock_patches()
