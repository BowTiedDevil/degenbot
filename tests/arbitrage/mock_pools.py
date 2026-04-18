"""
Mock pools for testing UniswapLpCycle without network dependencies.

These mocks satisfy the Pool protocol used by UniswapLpCycle while using
fixture states for calculations.
"""

import unittest.mock
from dataclasses import dataclass
from fractions import Fraction
from typing import TYPE_CHECKING, Any, Self, override

from eth_typing import ChecksumAddress

from degenbot.arbitrage.types import UniswapV2PoolSwapAmounts
from degenbot.arbitrage.uniswap_lp_cycle import UniswapLpCycle
from degenbot.erc20.erc20 import Erc20Token
from degenbot.exceptions.arbitrage import ArbitrageError
from degenbot.uniswap.types import UniswapPoolSwapVector
from degenbot.uniswap.v2_types import UniswapV2PoolState
from degenbot.uniswap.v3_types import UniswapV3PoolState
from degenbot.uniswap.v4_types import UniswapV4PoolState
from tests.arbitrage.generator.fixtures import ArbitrageCycleFixture

if TYPE_CHECKING:
    from degenbot.types.concrete import Subscriber


@dataclass(frozen=True, slots=True)
class MockErc20Token:
    """
    Minimal ERC20 token for testing.

    Frozen dataclass to ensure hashability and equality by address.
    """

    address: ChecksumAddress
    symbol: str = "TKN"
    decimals: int = 18
    chain_id: int = 1

    def __hash__(self) -> int:
        return hash(self.address)

    def __eq__(self, other: object) -> bool:
        if isinstance(other, MockErc20Token | Erc20Token):
            return self.address == other.address
        return False

    def __str__(self) -> str:
        return f"{self.symbol} ({self.address[:10]}...)"

    def __repr__(self) -> str:
        return f"MockErc20Token({self.address}, symbol={self.symbol!r})"


class MockV2Pool:
    """
    Mock V2 pool that satisfies UniswapLpCycle requirements.

    Does not connect to real contracts. Uses fixture states for calculations.

    Example
    -------
    >>> token0 = MockErc20Token(usdc_address, "USDC", 6)
    >>> token1 = MockErc20Token(weth_address, "WETH", 18)
    >>> state = UniswapV2PoolState(address=pool_address, block=0,
    ...                            reserves_token0=2000000000, reserves_token1=10**18)
    >>> pool = MockV2Pool(pool_address, token0, token1, state)
    >>> cycle = UniswapLpCycle(input_token=token0, swap_pools=[pool_a, pool_b])
    """

    def __init__(
        self,
        address: ChecksumAddress,
        token0: MockErc20Token | Erc20Token,
        token1: MockErc20Token | Erc20Token,
        initial_state: UniswapV2PoolState,
        fee: Fraction = Fraction(3, 1000),
    ) -> None:
        self.address = address
        self.token0 = token0
        self.token1 = token1
        self.tokens: tuple[MockErc20Token | Erc20Token, MockErc20Token | Erc20Token] = (
            token0,
            token1,
        )
        self.fee = fee
        # Fee for each direction (same for standard V2)
        self.fee_token0 = fee
        self.fee_token1 = fee
        self.chain_id = 1
        self.name = f"MockV2-{address[:10]}"
        self._state = initial_state
        self._subscribers: set[Subscriber] = set()

    @property
    @override
    def state(self) -> UniswapV2PoolState:
        return self._state

    def set_state(self, state: UniswapV2PoolState) -> None:
        """Update the pool state."""
        self._state = state

    def subscribe(self, subscriber: "Subscriber") -> Self:
        """Subscribe to pool updates (no-op for mock)."""
        self._subscribers.add(subscriber)
        return self

    def unsubscribe(self, subscriber: "Subscriber") -> Self:
        """Unsubscribe from pool updates (no-op for mock)."""
        self._subscribers.discard(subscriber)
        return self

    @staticmethod
    def swap_is_viable(
        state: UniswapV2PoolState,
        vector: UniswapPoolSwapVector,
    ) -> bool:
        """Check if swap is viable given state and direction."""
        if state.reserves_token0 == 0 or state.reserves_token1 == 0:
            return False
        return state.reserves_token1 > 1 if vector.zero_for_one else state.reserves_token0 > 1

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: MockErc20Token | Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """
        Calculate output tokens using x*y=k formula with fee.

        Parameters
        ----------
        token_in : MockErc20Token | Erc20Token
            The input token.
        token_in_quantity : int
            Amount of input tokens.
        override_state : UniswapV2PoolState | None
            Optional state to use instead of current state.

        Returns
        -------
        int
            Amount of output tokens.
        """
        state = override_state if override_state is not None else self._state

        # Determine which reserve is in/out
        if token_in == self.token0:
            reserve_in = state.reserves_token0
            reserve_out = state.reserves_token1
        else:
            reserve_in = state.reserves_token1
            reserve_out = state.reserves_token0

        if reserve_in == 0 or token_in_quantity == 0:
            return 0

        fee_multiplier = 1 - float(self.fee)
        amount_in_with_fee = int(token_in_quantity * fee_multiplier)
        numerator = amount_in_with_fee * reserve_out
        denominator = reserve_in + amount_in_with_fee

        result = numerator // denominator if denominator > 0 else 0
        # Ensure at least 1 output for non-trivial inputs to avoid zero swap amounts
        if result == 0 and token_in_quantity > 100:
            result = 1
        return result

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out: MockErc20Token | Erc20Token,
        token_out_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """Calculate input tokens needed for desired output."""
        state = override_state if override_state is not None else self._state

        if token_out == self.token1:
            reserve_in = state.reserves_token0
            reserve_out = state.reserves_token1
        else:
            reserve_in = state.reserves_token1
            reserve_out = state.reserves_token0

        if reserve_out <= token_out_quantity:
            return 0

        fee_multiplier = 1 - float(self.fee)
        numerator = reserve_in * token_out_quantity
        denominator = int((reserve_out - token_out_quantity) * fee_multiplier)

        return numerator // denominator + 1 if denominator > 0 else 0

    def get_absolute_exchange_rate(
        self,
        token: MockErc20Token | Erc20Token,
        override_state: UniswapV2PoolState | None = None,
    ) -> Fraction:
        """
        Get the absolute exchange rate for a token.

        Parameters
        ----------
        token : MockErc20Token | Erc20Token
            Token to get exchange rate for.
        override_state : UniswapV2PoolState | None
            Optional state override.

        Returns
        -------
        Fraction
            Exchange rate as a fraction.
        """
        state = override_state if override_state is not None else self._state

        if token == self.token1:
            # Exchange rate for token1: reserve0/reserve1
            if state.reserves_token1 == 0:
                return Fraction(0)
            return Fraction(state.reserves_token0, state.reserves_token1)
        # Exchange rate for token0: reserve1/reserve0
        if state.reserves_token0 == 0:
            return Fraction(0)
        return Fraction(state.reserves_token1, state.reserves_token0)

    def __hash__(self) -> int:
        return hash(self.address)

    def __eq__(self, other: object) -> bool:
        if isinstance(other, MockV2Pool):
            return self.address == other.address
        return False

    def __repr__(self) -> str:
        return f"MockV2Pool({self.address}, {self.token0.symbol}/{self.token1.symbol})"


class MockV3Pool:
    """
    Mock V3 pool that satisfies UniswapLpCycle requirements.

    Does not connect to real contracts. Uses fixture states for calculations.

    Note
    ----
    V3 calculations are simplified for testing. For accurate results, use
    real UniswapV3Pool with proper tick bitmap handling.
    """

    # Fee denominator for V3
    FEE_DENOMINATOR = 1_000_000

    def __init__(
        self,
        address: ChecksumAddress,
        token0: MockErc20Token | Erc20Token,
        token1: MockErc20Token | Erc20Token,
        initial_state: UniswapV3PoolState,
        tick_spacing: int = 60,
        fee: int = 3000,  # 0.3% in V3 format
    ) -> None:
        self.address = address
        self.token0 = token0
        self.token1 = token1
        self.tokens: tuple[MockErc20Token | Erc20Token, MockErc20Token | Erc20Token] = (
            token0,
            token1,
        )
        self.tick_spacing = tick_spacing
        self.fee = fee  # Fee in V3 format (e.g., 3000 = 0.3%)
        self.chain_id = 1
        self.name = f"MockV3-{address[:10]}"
        self._state = initial_state
        self._subscribers: set[Subscriber] = set()

        # Track sparse liquidity map (for compatibility check)
        self.sparse_liquidity_map: bool = False

    @property
    @override
    def state(self) -> UniswapV3PoolState:
        return self._state

    def set_state(self, state: UniswapV3PoolState) -> None:
        """Update the pool state."""
        self._state = state

    def subscribe(self, subscriber: "Subscriber") -> Self:
        """Subscribe to pool updates (no-op for mock)."""
        self._subscribers.add(subscriber)
        return self

    def unsubscribe(self, subscriber: "Subscriber") -> Self:
        """Unsubscribe from pool updates (no-op for mock)."""
        self._subscribers.discard(subscriber)
        return self

    @staticmethod
    def swap_is_viable(
        state: UniswapV3PoolState,
        vector: UniswapPoolSwapVector,
    ) -> bool:
        """Check if swap is viable given state and direction."""
        # V3 is viable if there's liquidity
        return state.liquidity > 0

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: MockErc20Token | Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV3PoolState | None = None,
    ) -> int:
        """
        Simplified V3 swap calculation.

        WARNING: This is a simplified implementation. Real V3 swaps require
        tick crossing logic. Use for testing structure only.

        Parameters
        ----------
        token_in : MockErc20Token | Erc20Token
            The input token.
        token_in_quantity : int
            Amount of input tokens.
        override_state : UniswapV3PoolState | None
            Optional state to use instead of current state.

        Returns
        -------
        int
            Amount of output tokens (approximate).
        """
        state = override_state if override_state is not None else self._state

        # Simplified: use sqrt price to estimate
        sqrt_price = state.sqrt_price_x96 / (2**96)
        price = sqrt_price * sqrt_price

        # Estimate output based on liquidity and price
        zero_for_one = token_in == self.token0

        if zero_for_one:
            # Selling token0 for token1
            # Approximate: amount_out ≈ amount_in * price * (1 - fee)
            amount_out = int(token_in_quantity * price * 0.997)
        else:
            # Selling token1 for token0
            amount_out = int(token_in_quantity / price * 0.997)

        # Cap by available liquidity
        max_out = int(state.liquidity) if zero_for_one else int(state.liquidity / price)
        return min(amount_out, max_out) if max_out > 0 else amount_out

    def get_absolute_exchange_rate(
        self,
        token: MockErc20Token | Erc20Token,
        override_state: UniswapV3PoolState | None = None,
    ) -> Fraction:
        """
        Get the absolute exchange rate for a token.

        Simplified implementation using sqrt price.
        """
        state = override_state if override_state is not None else self._state

        sqrt_price = state.sqrt_price_x96 / (2**96)
        price = sqrt_price * sqrt_price

        if token == self.token1:
            return Fraction(int(price * 10**18), 10**18)
        return Fraction(int(10**18 / price), 10**18) if price > 0 else Fraction(0)

    def __hash__(self) -> int:
        return hash(self.address)

    def __eq__(self, other: object) -> bool:
        if isinstance(other, MockV3Pool):
            return self.address == other.address
        return False

    def __repr__(self) -> str:
        return f"MockV3Pool({self.address}, {self.token0.symbol}/{self.token1.symbol})"


class MockV4Pool(MockV3Pool):
    """
    Mock V4 pool that satisfies UniswapLpCycle requirements.

    V4 pools are similar to V3 but with pool_id instead of address.
    """

    def __init__(
        self,
        address: ChecksumAddress,
        pool_id: bytes,
        token0: MockErc20Token | Erc20Token,
        token1: MockErc20Token | Erc20Token,
        initial_state: UniswapV4PoolState,
        tick_spacing: int = 60,
    ) -> None:
        super().__init__(address, token0, token1, initial_state, tick_spacing)
        self.pool_id = pool_id
        self.name = f"MockV4-{pool_id.hex()[:10]}"

    @property
    @override
    def state(self) -> UniswapV4PoolState:
        return self._state  # type: ignore[return-value]

    @staticmethod
    def swap_is_viable(
        state: UniswapV4PoolState,
        vector: UniswapPoolSwapVector,
    ) -> bool:
        """Check if swap is viable given state and direction."""
        return state.liquidity > 0

    def __hash__(self) -> int:
        return hash((self.address, self.pool_id))

    def __repr__(self) -> str:
        return f"MockV4Pool({self.pool_id.hex()[:10]}, {self.token0.symbol}/{self.token1.symbol})"


def build_mock_pool_from_state(
    address: ChecksumAddress,
    state: UniswapV2PoolState | UniswapV3PoolState | UniswapV4PoolState,
    token0: MockErc20Token | Erc20Token,
    token1: MockErc20Token | Erc20Token,
    pool_id: bytes | None = None,
) -> MockV2Pool | MockV3Pool | MockV4Pool:
    """
    Build a mock pool from a pool state.

    Parameters
    ----------
    address : ChecksumAddress
        Pool address (pool manager for V4).
    state : UniswapV2PoolState | UniswapV3PoolState | UniswapV4PoolState
        Pool state from fixture.
    token0 : MockErc20Token | Erc20Token
        Token0 for the pool.
    token1 : MockErc20Token | Erc20Token
        Token1 for the pool.
    pool_id : bytes | None
        Pool ID for V4 pools.

    Returns
    -------
    MockV2Pool | MockV3Pool | MockV4Pool
        Mock pool instance.
    """
    if isinstance(state, UniswapV2PoolState):
        return MockV2Pool(address, token0, token1, state)
    if isinstance(state, UniswapV3PoolState):
        return MockV3Pool(address, token0, token1, state)
    if isinstance(state, UniswapV4PoolState):
        if pool_id is None:
            msg = "pool_id required for V4 pools"
            raise ValueError(msg)
        return MockV4Pool(address, pool_id, token0, token1, state)

    msg = f"Unsupported state type: {type(state)}"
    raise TypeError(msg)


def build_mock_pools_from_fixture(
    fixture: ArbitrageCycleFixture,
    token0: MockErc20Token | Erc20Token,
    token1: MockErc20Token | Erc20Token,
) -> tuple[list[MockV2Pool | MockV3Pool | MockV4Pool], MockErc20Token | Erc20Token]:
    """
    Build mock pools from a fixture.

    Parameters
    ----------
    fixture : ArbitrageCycleFixture
        The fixture containing pool states.
    token0 : MockErc20Token | Erc20Token
        Token0 (e.g., USDC).
    token1 : MockErc20Token | Erc20Token
        Token1 (e.g., WETH).

    Returns
    -------
    tuple[list[MockV2Pool | MockV3Pool | MockV4Pool], MockErc20Token | Erc20Token]
        List of mock pools and the input token.
    """
    pools: list[MockV2Pool | MockV3Pool | MockV4Pool] = []
    input_token: MockErc20Token | Erc20Token | None = None

    for address, state in fixture.pool_states.items():
        # Build mock pool from state
        pool_id = None
        if isinstance(state, UniswapV4PoolState):
            pool_id = state.id

        pool = build_mock_pool_from_state(
            address=address,
            state=state,
            token0=token0,
            token1=token1,
            pool_id=pool_id,
        )
        pools.append(pool)

        # Determine input token
        if input_token is None:
            input_token = token0 if fixture.input_token_address == token0.address else token1

    input_token = input_token if input_token is not None else token0

    return pools, input_token


def create_cycle_with_mocks(
    fixture: ArbitrageCycleFixture,
    token0: MockErc20Token | Erc20Token,
    token1: MockErc20Token | Erc20Token,
    cycle_id: str = "test_cycle",
    max_input: int = 10**21,
) -> "tuple[UniswapLpCycle, list[MockV2Pool | MockV3Pool | MockV4Pool]]":
    """
    Create a UniswapLpCycle with mock pools from a fixture.

    Patches the pool validation to accept mock pools.

    Parameters
    ----------
    fixture : ArbitrageCycleFixture
        The fixture to use.
    token0 : MockErc20Token | Erc20Token
        Token0 for pools.
    token1 : MockErc20Token | Erc20Token
        Token1 for pools.
    cycle_id : str
        ID for the cycle.
    max_input : int
        Maximum input amount.

    Returns
    -------
    tuple[UniswapLpCycle, list[MockV2Pool | MockV3Pool | MockV4Pool]]
        The cycle and list of mock pools.
    """

    pools, input_token = build_mock_pools_from_fixture(fixture, token0, token1)

    # Patch _pool_is_viable to handle mock pools
    def patched_pool_is_viable(
        pool: MockV2Pool | MockV3Pool | MockV4Pool,
        state: UniswapV2PoolState | UniswapV3PoolState | UniswapV4PoolState,
        vector: UniswapPoolSwapVector,
    ) -> bool:
        """Patched viability check for mock pools."""
        if isinstance(pool, MockV2Pool):
            return pool.swap_is_viable(state, vector)
        if isinstance(pool, MockV3Pool | MockV4Pool):
            return pool.swap_is_viable(state, vector)
        return True

    def patched_pre_calculation_check(
        self: UniswapLpCycle,
        min_rate_of_exchange: Fraction = Fraction(1, 1),
        state_overrides: dict[MockV2Pool | MockV3Pool | MockV4Pool, Any] | None = None,
    ) -> None:
        """Patched pre-calculation check for mock pools."""
        if state_overrides is None:
            state_overrides = {}

        # Check viability
        for pool, vector in zip(self.swap_pools, self._swap_vectors, strict=True):
            state = state_overrides.get(pool, pool.state)
            if not patched_pool_is_viable(pool, state, vector):
                msg = f"Pool {pool} is not viable"
                raise ValueError(msg)

    def patched_build_swap_amounts(
        self: UniswapLpCycle,
        token_in_quantity: int,
        state_overrides: dict[MockV2Pool | MockV3Pool | MockV4Pool, Any] | None = None,
    ) -> tuple[UniswapV2PoolSwapAmounts, ...]:
        """Patched build_swap_amounts for mock pools."""
        if state_overrides is None:
            state_overrides = {}

        token_out_quantity = 0
        swap_amounts: list[UniswapV2PoolSwapAmounts] = []

        for pool, swap_vector in zip(self.swap_pools, self._swap_vectors, strict=True):
            if token_in_quantity == 0:
                raise ArbitrageError(message="A swap would result in an output of zero.")

            pool_state = state_overrides.get(pool)

            token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                token_in=swap_vector.token_in,
                token_in_quantity=token_in_quantity,
                override_state=pool_state,
            )

            # Skip creating swap amounts if output is zero
            if token_out_quantity == 0:
                raise ArbitrageError(message=f"Swap produced zero output for pool {pool.address}")

            swap_amounts.append(
                UniswapV2PoolSwapAmounts(
                    pool=pool.address,
                    amounts_in=(token_in_quantity, 0)
                    if swap_vector.zero_for_one
                    else (0, token_in_quantity),
                    amounts_out=(0, token_out_quantity)
                    if swap_vector.zero_for_one
                    else (token_out_quantity, 0),
                )
            )
            token_in_quantity = token_out_quantity

        return tuple(swap_amounts)

    # Keep patches applied - we need to store them
    UniswapLpCycle._mock_patch_validate = unittest.mock.patch.object(
        UniswapLpCycle, "_validate_pools", return_value=None
    )
    UniswapLpCycle._mock_patch_viable = unittest.mock.patch.object(
        UniswapLpCycle, "_pool_is_viable", staticmethod(patched_pool_is_viable)
    )
    UniswapLpCycle._mock_patch_pre_calc = unittest.mock.patch.object(
        UniswapLpCycle, "_pre_calculation_check", patched_pre_calculation_check
    )
    UniswapLpCycle._mock_patch_build_swap = unittest.mock.patch.object(
        UniswapLpCycle, "_build_swap_amounts", patched_build_swap_amounts
    )

    # Start patches
    UniswapLpCycle._mock_patch_validate.__enter__()
    UniswapLpCycle._mock_patch_viable.__enter__()
    UniswapLpCycle._mock_patch_pre_calc.__enter__()
    UniswapLpCycle._mock_patch_build_swap.__enter__()

    try:
        cycle = UniswapLpCycle(
            id=cycle_id,
            input_token=input_token,
            swap_pools=pools,
            max_input=max_input,
        )
    except Exception:
        # Clean up patches on error
        UniswapLpCycle._mock_patch_validate.__exit__(None, None, None)
        UniswapLpCycle._mock_patch_viable.__exit__(None, None, None)
        UniswapLpCycle._mock_patch_pre_calc.__exit__(None, None, None)
        UniswapLpCycle._mock_patch_build_swap.__exit__(None, None, None)
        raise

    return cycle, pools


def cleanup_mock_patches() -> None:
    """Clean up mock patches on UniswapLpCycle."""

    for attr in [
        "_mock_patch_validate",
        "_mock_patch_viable",
        "_mock_patch_pre_calc",
        "_mock_patch_build_swap",
    ]:
        if hasattr(UniswapLpCycle, attr):
            getattr(UniswapLpCycle, attr).__exit__(None, None, None)
            delattr(UniswapLpCycle, attr)
