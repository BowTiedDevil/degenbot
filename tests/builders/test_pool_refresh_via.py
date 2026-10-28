"""Behavioral tests for the core-owned pool refresh entry (`Pool.refresh_via`).

The update path collapsed onto ONE core entry: the per-family
fetch/compare/apply choreography lives in ``degenbot_bot::bot_core::refresh``,
and the driver reaches it through ``AbstractLiquidityPool.refresh_via`` ->
``Pool.refresh_via``.

A real ``BotIo`` over a recorded ``OfflineProvider`` stands in for the chain:
the Rust choreography is alloy-only, so the recorded call is keyed by the
alloy transport's exact-calldata form (``{addr}:0x{data}``).
"""

from __future__ import annotations

from fractions import Fraction

import pytest

from degenbot.provider import OfflineProvider
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.seam import Bot, BotIo
from tests.helpers.v2_pool_factory import make_v2_pool

_TOKEN_BOT = Bot()

_POOL_ADDRESS = "0x0000000000000000000000000000000000000001"
_TOKEN0_ADDRESS = "0x0000000000000000000000000000000000000002"
_TOKEN1_ADDRESS = "0x0000000000000000000000000000000000000003"
_FACTORY_ADDRESS = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
_INIT_HASH = "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"

_BLOCK = 1

# `getReserves()` selector — the one recorded call a V2 refresh issues.
_GET_RESERVES_SELECTOR = "0x0902f1ac"


def _mock_token(address: str, *, symbol: str, decimals: int = 18) -> object:
    """Build a real Erc20Token for refresh tests (needs real metadata for ADR-006)."""
    return make_erc20(
        _TOKEN_BOT,
        address,
        chain_id=1,
        name=symbol,
        symbol=symbol,
        decimals=decimals,
    )


def _word(value: int) -> str:
    """One ABI-encoded 32-byte word, hex (no `0x`)."""
    return value.to_bytes(32, "big").hex()


def _reserves_provider(reserve0: int, reserve1: int, *, block: int = _BLOCK) -> OfflineProvider:
    """A one-block provider serving `getReserves()` (uint112, uint112, uint32)."""
    to = _POOL_ADDRESS.lower()
    reserves = _word(reserve0) + _word(reserve1) + _word(0)
    return OfflineProvider(
        chain_id=1,
        blocks={
            str(block): {
                "timestamp": 1_700_000_000,
                "calls": {f"{to}:{_GET_RESERVES_SELECTOR}": reserves},
                "code": {},
            }
        },
    )


class TestPoolRefreshV2:
    """`refresh_via` drives the core V2 refresh: fetch, compare, apply."""

    def _pool(self) -> object:
        return make_v2_pool(
            address=_POOL_ADDRESS,
            chain_id=1,
            token0=_mock_token(_TOKEN0_ADDRESS, symbol="TK0"),
            token1=_mock_token(_TOKEN1_ADDRESS, symbol="TK1", decimals=6),
            factory=_FACTORY_ADDRESS,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000,
            reserves_token1=2000,
            state_block=_BLOCK,
            deployer_address=_FACTORY_ADDRESS,
            init_hash=_INIT_HASH,
        )

    def test_refresh_returns_true_and_applies_changed_reserves(self) -> None:
        io = BotIo(provider=_reserves_provider(5000, 6000))
        pool = self._pool()
        assert pool.refresh_via(io, block_number=_BLOCK) is True
        assert pool.reserves_token0 == 5000
        assert pool.reserves_token1 == 6000

    def test_refresh_returns_false_when_reserves_match(self) -> None:
        io = BotIo(provider=_reserves_provider(1000, 2000))
        pool = self._pool()
        assert pool.refresh_via(io, block_number=_BLOCK) is False
        assert pool.reserves_token0 == 1000
        assert pool.reserves_token1 == 2000

    def test_refresh_defaults_to_the_chain_head(self) -> None:
        """No explicit block: the core reads the head, which the recording serves."""
        io = BotIo(provider=_reserves_provider(5000, 6000))
        pool = self._pool()
        assert pool.refresh_via(io) is True
        assert pool.reserves_token0 == 5000

    def test_refresh_resolves_the_latest_tag_at_the_seam(self) -> None:
        """A block tag is a typed value: "latest" reads the recorded head (block 1)."""
        io = BotIo(provider=_reserves_provider(5000, 6000))
        pool = self._pool()
        assert pool.refresh_via(io, block_number="latest") is True
        assert pool.reserves_token0 == 5000

    def test_refresh_refuses_a_string_that_is_not_a_tag(self) -> None:
        """A decimal string is not a block tag — it is refused, not coerced to a number."""
        io = BotIo(provider=_reserves_provider(5000, 6000))
        pool = self._pool()
        with pytest.raises(ValueError, match="unsupported block identifier"):
            pool.refresh_via(io, block_number="123")


class TestBotIoBlockTagSeam:
    """The `BotIo` block argument is a typed selector at the seam."""

    def test_page_read_refuses_a_string_that_is_not_a_tag(self) -> None:
        io = BotIo(provider=_reserves_provider(5000, 6000))
        with pytest.raises(ValueError, match="unsupported block identifier"):
            io.get_code(_POOL_ADDRESS, block="0x10")
