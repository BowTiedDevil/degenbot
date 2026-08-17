"""Tests for builder update() method — structural conformance and behavioral integration.

Plan 074: Verifies that all remaining builder update() methods are
@staticmethod and dispatch correctly through both class and instance call
patterns. Also verifies the I/O flows exclusively through the io parameter,
not through self.

T4 / 4GQWZ4: the V2/V3/V4 builders are retired — their refresh logic now lives
in `Bot.update()` → `_update_pool` (degenbot.bot._bot), so the V2 behavioral
integration test exercises that dispatcher directly rather than a builder.

Post ADR-005 slice-14 collapse: builders call ``io.fetch_X()`` directly
(the Python ``io.call()`` parity-gate fallback is retired), so the
behavioral fake is a duck-typed object exposing the ``fetch_*`` methods
the tested path invokes — no ``BotIo`` subclass needed (Q4 alpha).
"""

from __future__ import annotations

import inspect
from fractions import Fraction
from unittest.mock import MagicMock

import pytest

from degenbot._ffi import Bot
from degenbot.bot._bot import _update_pool
from degenbot.builders.balancer_builder import BalancerBuilder
from degenbot.builders.curve_pool_builder import CurvePoolBuilder
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

# --- Helpers ---


_TOKEN_BOT = Bot()


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


class FakePyBotIo:
    """Duck-typed BotIo stand-in for V2 update() behavior tests.

    Exposes only the seam methods ``_update_pool`` actually invokes for V2
    (``get_block_number`` + ``fetch_v2_reserves``). The dispatcher never
    ``isinstance(io, BotIo)`` — it duck-types the ``fetch_*`` calls.
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
    """update() is a @staticmethod on every remaining builder — no self injection."""

    @pytest.mark.parametrize(
        "builder_class",
        [
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


class TestBalancerBuilderUpdateClassVsInstanceCall:
    """update() is callable both as a class method and on an instance,
    matching the Bot.update() dispatch pattern.
    """

    def test_balancer_builder_update_callable_on_class(self) -> None:
        """BalancerBuilder.update is callable on the class."""
        method = BalancerBuilder.update
        assert callable(method)
        sig = inspect.signature(method)
        params = list(sig.parameters.keys())
        assert params[0] == "pool"


# --- Behavioral integration tests (relocated V2 refresh) ---


class TestUpdatePoolV2Behavior:
    """`_update_pool` (relocated off the retired V2 builder) dispatches V2
    refresh through the io seam and pushes to the pool."""

    def _pool(self) -> object:
        return make_v2_pool(
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

    def test_update_returns_true_when_state_changes(self) -> None:
        """_update_pool() returns True and calls external_update when reserves change."""
        io = FakePyBotIo(reserves0=5000, reserves1=6000)
        pool = self._pool()
        result = _update_pool(pool, block_number=1, io=io)
        assert result is True
        assert pool.reserves_token0 == 5000
        assert pool.reserves_token1 == 6000

    def test_update_returns_false_when_state_unchanged(self) -> None:
        """_update_pool() returns False when reserves match current state."""
        io = FakePyBotIo(reserves0=1000, reserves1=2000)
        pool = self._pool()
        result = _update_pool(pool, block_number=1, io=io)
        assert result is False

    def test_update_rejects_non_v2_v3_v4(self) -> None:
        """_update_pool() raises TypeError for an unsupported pool type."""
        with pytest.raises(TypeError, match="_update_pool cannot update"):
            _update_pool(MagicMock(), block_number=1, io=MagicMock())
