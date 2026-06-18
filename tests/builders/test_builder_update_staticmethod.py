"""Tests for builder update() method — structural conformance and behavioral integration.

Plan 074: Verifies that all builder update() methods are @staticmethod
and dispatch correctly through both class and instance call patterns.
Also verifies the I/O flows exclusively through the io parameter,
not through self.
"""

from __future__ import annotations

import inspect
from fractions import Fraction
from unittest.mock import MagicMock

import eth_abi.abi
import pytest
from hexbytes import HexBytes
from web3 import Web3

from degenbot.builders.aerodrome_v2_builder import AerodromeV2Builder
from degenbot.builders.async_context import AsyncBuilderContext
from degenbot.builders.async_v2_pool_builder import AsyncV2PoolBuilder
from degenbot.builders.async_v3_pool_builder import AsyncV3PoolBuilder
from degenbot.builders.async_v4_pool_builder import AsyncV4PoolBuilder
from degenbot.builders.balancer_builder import BalancerBuilder
from degenbot.builders.camelot_builder import CamelotBuilder
from degenbot.builders.context import BuilderContext
from degenbot.builders.curve_pool_builder import CurvePoolBuilder
from degenbot.builders.pool_io import AsyncPoolIO, SyncPoolIO
from degenbot.builders.v2_pool_builder import V2PoolBuilder
from degenbot.builders.v3_pool_builder import V3PoolBuilder
from degenbot.builders.v4_pool_builder import V4PoolBuilder
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.degenbot_rs import PyBot
from degenbot.registry import PoolRegistry, TokenRegistry
from degenbot.uniswap.liquidity_pool import LiquidityPool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from tests.helpers.v2_pool_factory import make_v2_pool

# --- Helpers ---


def _selector(signature: str) -> str:
    """Return the 4-byte function selector as a hex string (no 0x prefix)."""
    return Web3.keccak(text=signature)[:4].hex()


def _fake_builder_context() -> BuilderContext:
    """Create a BuilderContext with mock dependencies."""
    erc20_builder = MagicMock()
    db = MagicMock(spec=DatabaseSessionManager)
    db.side_effect = RuntimeError("no db")
    return BuilderContext(
        db=db,
        pools=MagicMock(spec=PoolRegistry),
        tokens=MagicMock(spec=TokenRegistry),
        erc20_builder=erc20_builder,
        py_bot=PyBot(),
        default_chain_id=1,
        managed_pools=MagicMock(),
    )


def _fake_async_builder_context() -> AsyncBuilderContext:
    """Create an AsyncBuilderContext with mock dependencies."""
    erc20_builder = MagicMock()
    db = MagicMock(spec=DatabaseSessionManager)
    db.side_effect = RuntimeError("no db")
    return AsyncBuilderContext(
        db=db,
        pools=MagicMock(spec=PoolRegistry),
        tokens=MagicMock(spec=TokenRegistry),
        erc20_builder=erc20_builder,
        default_chain_id=1,
    )


# --- Structural conformance tests ---


class TestUpdateIsStaticMethod:
    """update() is a @staticmethod on every builder — no self injection."""

    @pytest.mark.parametrize(
        "builder_class",
        [
            V2PoolBuilder,
            AerodromeV2Builder,
            CamelotBuilder,
            V3PoolBuilder,
            V4PoolBuilder,
            CurvePoolBuilder,
            BalancerBuilder,
        ],
    )
    def test_sync_builder_update_is_staticmethod(self, builder_class: type) -> None:
        """Sync builder update() is declared as @staticmethod."""
        assert isinstance(
            inspect.getattr_static(builder_class, "update"),
            staticmethod,
        ), f"{builder_class.__name__}.update is not a @staticmethod"

    @pytest.mark.parametrize(
        "builder_class",
        [
            AsyncV2PoolBuilder,
            AsyncV3PoolBuilder,
            AsyncV4PoolBuilder,
        ],
    )
    def test_async_builder_update_is_staticmethod(self, builder_class: type) -> None:
        """Async builder update() is declared as @staticmethod."""
        assert isinstance(
            inspect.getattr_static(builder_class, "update"),
            staticmethod,
        ), f"{builder_class.__name__}.update is not a @staticmethod"


class TestUpdateClassVsInstanceCall:
    """update() is callable both as a class method and on an instance,
    matching the Bot.update() dispatch pattern.
    """

    def test_v2_builder_update_callable_on_class(self) -> None:
        """V2PoolBuilder.update is callable on the class (no instance needed)."""
        # We can't call it without a real pool/io, but we can verify it's
        # accessible and has the right signature
        method = V2PoolBuilder.update
        assert callable(method)
        sig = inspect.signature(method)
        # First param should be 'pool', not 'self'
        params = list(sig.parameters.keys())
        assert params[0] == "pool"

    def test_v2_builder_update_callable_on_instance(self) -> None:
        """V2PoolBuilder.update is callable on an instance (matches Bot dispatch)."""
        builder = V2PoolBuilder(_fake_builder_context())
        method = builder.update
        assert callable(method)

    def test_balancer_builder_update_callable_on_class(self) -> None:
        """BalancerBuilder.update is callable on the class."""
        method = BalancerBuilder.update
        assert callable(method)
        sig = inspect.signature(method)
        params = list(sig.parameters.keys())
        assert params[0] == "pool"

    def test_async_v2_builder_update_callable_on_class(self) -> None:
        """AsyncV2PoolBuilder.update is callable on the class."""
        method = AsyncV2PoolBuilder.update
        assert callable(method)
        sig = inspect.signature(method)
        params = list(sig.parameters.keys())
        assert params[0] == "pool"


# --- Behavioral integration tests ---


class FakeSyncProvider:
    """Sync fake that returns pre-programmed ABI-encoded responses."""

    def __init__(self, responses: dict[str, bytes]) -> None:
        self._responses = responses

    def make_request(
        self, method: str, params: list
    ) -> HexBytes:
        if method == "eth_getCode":
            return HexBytes(b"\x60\x00")
        msg = f"Unexpected method: {method}"
        raise ValueError(msg)

    def call(
        self, *, to: str, data: bytes, block: int | None = None
    ) -> HexBytes:
        selector = data[:4].hex()
        if selector in self._responses:
            return HexBytes(self._responses[selector])
        msg = f"No mock response for selector 0x{selector}"
        raise ValueError(msg)

    def get_block_number(self) -> int:
        return 1


class FakeAsyncProvider:
    """Async fake that returns pre-programmed ABI-encoded responses."""

    def __init__(self, responses: dict[str, bytes]) -> None:
        self._responses = responses

    async def call(
        self, *, to: str, data: bytes, block: int | None = None
    ) -> HexBytes:
        selector = data[:4].hex()
        if selector in self._responses:
            return HexBytes(self._responses[selector])
        msg = f"No mock response for selector 0x{selector}"
        raise ValueError(msg)

    async def get_block_number(self) -> int:
        return 1


def _v2_update_responses(
    *,
    reserves0: int = 5000,
    reserves1: int = 6000,
) -> dict[str, bytes]:
    """Build responses for V2 update (getReserves only)."""
    return {
        _selector("getReserves()"): eth_abi.abi.encode(
            ["uint256", "uint256"], [reserves0, reserves1]
        ),
    }


class TestV2BuilderUpdateBehavior:
    """V2PoolBuilder.update() dispatches through io, pushes to pool."""

    def test_update_returns_true_when_state_changes(self) -> None:
        """update() returns True and calls external_update when reserves change."""
        provider = FakeSyncProvider(_v2_update_responses(reserves0=5000, reserves1=6000))
        io = SyncPoolIO(provider)

        # Create a real V2 pool with different reserves
        pool = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            chain_id=1,
            token0=MagicMock(address="0x0000000000000000000000000000000000000002", chain_id=1),
            token1=MagicMock(address="0x0000000000000000000000000000000000000003", chain_id=1),
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000,
            reserves_token1=2000,
            state_block=1,
            deployer_address="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            init_hash=LiquidityPool.UNISWAP_V2_MAINNET_POOL_INIT_HASH,
        )

        builder = V2PoolBuilder(_fake_builder_context())
        result = builder.update(pool, io=io, block_number=1)

        assert result is True
        assert pool.reserves_token0 == 5000
        assert pool.reserves_token1 == 6000

    def test_update_returns_false_when_state_unchanged(self) -> None:
        """update() returns False when reserves match current state."""
        provider = FakeSyncProvider(_v2_update_responses(reserves0=1000, reserves1=2000))
        io = SyncPoolIO(provider)

        pool = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            chain_id=1,
            token0=MagicMock(address="0x0000000000000000000000000000000000000002", chain_id=1),
            token1=MagicMock(address="0x0000000000000000000000000000000000000003", chain_id=1),
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000,
            reserves_token1=2000,
            state_block=1,
            deployer_address="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            init_hash=LiquidityPool.UNISWAP_V2_MAINNET_POOL_INIT_HASH,
        )

        builder = V2PoolBuilder(_fake_builder_context())
        result = builder.update(pool, io=io, block_number=1)

        assert result is False

    def test_update_callable_on_class(self) -> None:
        """update() works when called on the class (no instance needed)."""
        provider = FakeSyncProvider(_v2_update_responses(reserves0=5000, reserves1=6000))
        io = SyncPoolIO(provider)

        pool = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            chain_id=1,
            token0=MagicMock(address="0x0000000000000000000000000000000000000002", chain_id=1),
            token1=MagicMock(address="0x0000000000000000000000000000000000000003", chain_id=1),
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000,
            reserves_token1=2000,
            state_block=1,
            deployer_address="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            init_hash=LiquidityPool.UNISWAP_V2_MAINNET_POOL_INIT_HASH,
        )

        # Class-level call — no builder instance needed
        result = V2PoolBuilder.update(pool, io=io, block_number=1)

        assert result is True
        assert pool.reserves_token0 == 5000
        assert pool.reserves_token1 == 6000


class TestAsyncV2BuilderUpdateBehavior:
    """AsyncV2PoolBuilder.update() dispatches through io, pushes to pool."""

    @pytest.mark.asyncio
    async def test_async_update_returns_true_when_state_changes(self) -> None:
        """Async update() returns True and calls external_update when reserves change."""
        provider = FakeAsyncProvider(_v2_update_responses(reserves0=5000, reserves1=6000))
        io = AsyncPoolIO(provider)

        pool = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            chain_id=1,
            token0=MagicMock(address="0x0000000000000000000000000000000000000002", chain_id=1),
            token1=MagicMock(address="0x0000000000000000000000000000000000000003", chain_id=1),
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000,
            reserves_token1=2000,
            state_block=1,
            deployer_address="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            init_hash=LiquidityPool.UNISWAP_V2_MAINNET_POOL_INIT_HASH,
        )

        ctx = _fake_async_builder_context()
        builder = AsyncV2PoolBuilder(ctx)
        result = await builder.update(pool, io=io, block_number=1)

        assert result is True
        assert pool.reserves_token0 == 5000
        assert pool.reserves_token1 == 6000

    @pytest.mark.asyncio
    async def test_async_update_returns_false_when_state_unchanged(self) -> None:
        """Async update() returns False when reserves match current state."""
        provider = FakeAsyncProvider(_v2_update_responses(reserves0=1000, reserves1=2000))
        io = AsyncPoolIO(provider)

        pool = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            chain_id=1,
            token0=MagicMock(address="0x0000000000000000000000000000000000000002", chain_id=1),
            token1=MagicMock(address="0x0000000000000000000000000000000000000003", chain_id=1),
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000,
            reserves_token1=2000,
            state_block=1,
            deployer_address="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            init_hash=LiquidityPool.UNISWAP_V2_MAINNET_POOL_INIT_HASH,
        )

        ctx = _fake_async_builder_context()
        builder = AsyncV2PoolBuilder(ctx)
        result = await builder.update(pool, io=io, block_number=1)

        assert result is False

    @pytest.mark.asyncio
    async def test_async_update_callable_on_class(self) -> None:
        """Async update() works when called on the class (no instance needed)."""
        provider = FakeAsyncProvider(_v2_update_responses(reserves0=5000, reserves1=6000))
        io = AsyncPoolIO(provider)

        pool = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            chain_id=1,
            token0=MagicMock(address="0x0000000000000000000000000000000000000002", chain_id=1),
            token1=MagicMock(address="0x0000000000000000000000000000000000000003", chain_id=1),
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000,
            reserves_token1=2000,
            state_block=1,
            deployer_address="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            init_hash=LiquidityPool.UNISWAP_V2_MAINNET_POOL_INIT_HASH,
        )

        # Class-level call — no builder instance needed
        result = await AsyncV2PoolBuilder.update(pool, io=io, block_number=1)

        assert result is True
        assert pool.reserves_token0 == 5000
        assert pool.reserves_token1 == 6000


class TestBuilderUpdateRejectsWrongPoolType:
    """update() raises TypeError when given a pool of the wrong type."""

    def test_v2_builder_rejects_v3_pool(self) -> None:
        """V2PoolBuilder.update raises TypeError for a V3 pool."""
        provider = FakeSyncProvider({})
        io = SyncPoolIO(provider)

        pool = MagicMock(spec=UniswapV3Pool)

        builder = V2PoolBuilder(_fake_builder_context())
        with pytest.raises(TypeError, match="V2PoolBuilder cannot update"):
            builder.update(pool, io=io, block_number=1)

    def test_v3_builder_rejects_v2_pool(self) -> None:
        """V3PoolBuilder.update raises TypeError for a V2 pool."""
        provider = FakeSyncProvider({})
        io = SyncPoolIO(provider)

        pool = MagicMock(spec=LiquidityPool)

        builder = V3PoolBuilder(_fake_builder_context())
        with pytest.raises(TypeError, match="V3PoolBuilder cannot update"):
            builder.update(pool, io=io, block_number=1)
