"""Tests for builder update() method — structural conformance and behavioral integration.

Plan 074: Verifies that all builder update() methods are @staticmethod
and dispatch correctly through both class and instance call patterns.
Also verifies the I/O flows exclusively through the io parameter,
not through self.

Post ADR-005 slice-14 collapse: builders call ``io.fetch_X()`` directly
(the Python ``io.call()`` parity-gate fallback is retired), so the
behavioral fake is a duck-typed object exposing the ``fetch_*`` methods
the tested path invokes — no ``PyBotIo`` subclass needed (Q4 alpha).
"""

from __future__ import annotations

import inspect
from fractions import Fraction
from unittest.mock import MagicMock

import pytest

from degenbot.bot import PyBot
from degenbot.builders.aerodrome_v2_builder import AerodromeV2Builder
from degenbot.builders.balancer_builder import BalancerBuilder
from degenbot.builders.context import BuilderContext
from degenbot.builders.curve_pool_builder import CurvePoolBuilder
from degenbot.builders.v2_pool_builder import V2PoolBuilder
from degenbot.builders.v3_pool_builder import V3PoolBuilder
from degenbot.builders.v4_pool_builder import V4PoolBuilder
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.registry import PoolRegistry, TokenRegistry
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

# --- Helpers ---


_TOKEN_BOT = PyBot()


def _mock_token(address: str, *, symbol: str, decimals: int = 18) -> object:
    """Build a real Erc20Token for update tests (needs real metadata for ADR-006)."""
    return make_erc20(
        _TOKEN_BOT,
        address,
        chain_id=1,
        name=symbol,
        symbol=symbol,
        decimals=decimals,
    )


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


class FakePyBotIo:
    """Duck-typed PyBotIo stand-in for V2 update() behavior tests.

    Exposes only the seam methods ``V2PoolBuilder.update`` actually invokes
    (``get_block_number`` + ``fetch_v2_reserves``). Builders never
    ``isinstance(io, PyBotIo)`` — they duck-type the ``fetch_*`` calls.
    """

    def __init__(self, *, reserves0: int = 5000, reserves1: int = 6000) -> None:
        self._reserves0 = reserves0
        self._reserves1 = reserves1

    def get_block_number(self) -> int:
        return 1

    def fetch_v2_reserves(self, _address: str, block: int | None = None) -> tuple[int, int]:
        return self._reserves0, self._reserves1


# --- Structural conformance tests ---


class TestUpdateIsStaticMethod:
    """update() is a @staticmethod on every builder — no self injection."""

    @pytest.mark.parametrize(
        "builder_class",
        [
            V2PoolBuilder,
            AerodromeV2Builder,
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


class TestUpdateClassVsInstanceCall:
    """update() is callable both as a class method and on an instance,
    matching the Bot.update() dispatch pattern.
    """

    def test_v2_builder_update_callable_on_class(self) -> None:
        """V2PoolBuilder.update is callable on the class (no instance needed)."""
        method = V2PoolBuilder.update
        assert callable(method)
        sig = inspect.signature(method)
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


# --- Behavioral integration tests ---


class TestV2BuilderUpdateBehavior:
    """V2PoolBuilder.update() dispatches through io, pushes to pool."""

    def test_update_returns_true_when_state_changes(self) -> None:
        """update() returns True and calls external_update when reserves change."""
        io = FakePyBotIo(reserves0=5000, reserves1=6000)

        pool = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            chain_id=1,
            token0=_mock_token("0x0000000000000000000000000000000000000002", symbol="TK0"),
            token1=_mock_token(
                "0x0000000000000000000000000000000000000003", symbol="TK1", decimals=6
            ),
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000,
            reserves_token1=2000,
            state_block=1,
            deployer_address="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            init_hash="0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
        )

        builder = V2PoolBuilder(_fake_builder_context())
        result = builder.update(pool, io=io, block_number=1)

        assert result is True
        assert pool.reserves_token0 == 5000
        assert pool.reserves_token1 == 6000

    def test_update_returns_false_when_state_unchanged(self) -> None:
        """update() returns False when reserves match current state."""
        io = FakePyBotIo(reserves0=1000, reserves1=2000)

        pool = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            chain_id=1,
            token0=_mock_token("0x0000000000000000000000000000000000000002", symbol="TK0"),
            token1=_mock_token(
                "0x0000000000000000000000000000000000000003", symbol="TK1", decimals=6
            ),
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000,
            reserves_token1=2000,
            state_block=1,
            deployer_address="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            init_hash="0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
        )

        builder = V2PoolBuilder(_fake_builder_context())
        result = builder.update(pool, io=io, block_number=1)

        assert result is False

    def test_update_callable_on_class(self) -> None:
        """update() works when called on the class (no instance needed)."""
        io = FakePyBotIo(reserves0=5000, reserves1=6000)

        pool = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            chain_id=1,
            token0=_mock_token("0x0000000000000000000000000000000000000002", symbol="TK0"),
            token1=_mock_token(
                "0x0000000000000000000000000000000000000003", symbol="TK1", decimals=6
            ),
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000,
            reserves_token1=2000,
            state_block=1,
            deployer_address="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            init_hash="0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
        )

        # Class-level call — no builder instance needed
        result = V2PoolBuilder.update(pool, io=io, block_number=1)

        assert result is True
        assert pool.reserves_token0 == 5000
        assert pool.reserves_token1 == 6000


class TestBuilderUpdateRejectsWrongPoolType:
    """update() raises TypeError when given a pool of the wrong type.

    The type check runs before any I/O, so a bare MagicMock io suffices.
    """

    def test_v2_builder_rejects_v3_pool(self) -> None:
        """V2PoolBuilder.update raises TypeError for a V3 pool."""
        io = MagicMock()
        pool = MagicMock(spec=UniswapV3Pool)

        builder = V2PoolBuilder(_fake_builder_context())
        with pytest.raises(TypeError, match="V2PoolBuilder cannot update"):
            builder.update(pool, io=io, block_number=1)

    def test_v3_builder_rejects_v2_pool(self) -> None:
        """V3PoolBuilder.update raises TypeError for a V2 pool."""
        io = MagicMock()
        pool = MagicMock(spec=UniswapV2Pool)

        builder = V3PoolBuilder(_fake_builder_context())
        with pytest.raises(TypeError, match="V3PoolBuilder cannot update"):
            builder.update(pool, io=io, block_number=1)
