"""UniswapV2Pool direct construction is forbidden (ADR-005 Polars-style seam).

A ``UniswapV2Pool`` is a Python companion over a Rust-owned
``PyLiquidityPool`` handle. The handle can only be produced by registering a
pool in a ``PyBot`` (production: ``Bot.build_pool()``; tests: ``make_v2_pool``).
Direct constructor calls are rejected so that the only paths to a pool instance
are the ones that wire the handle — mirroring Polars' ``_from_pydf`` pattern.
"""

from fractions import Fraction
from unittest.mock import MagicMock

import pytest

from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool


class TestDirectConstructionForbidden:
    """``UniswapV2Pool(...)`` raises ``TypeError`` with a helpful message."""

    def test_no_args_raises_type_error(self) -> None:
        """Calling the constructor with no arguments is rejected."""
        with pytest.raises(TypeError) as exc_info:
            UniswapV2Pool()

        message = str(exc_info.value)
        assert "Bot.build_pool" in message
        assert "make_v2_pool" in message

    def test_with_py_pool_handle_raises_type_error(self) -> None:
        """Passing a fake ``PyLiquidityPool`` handle (the old call shape) is rejected."""
        fake_handle = MagicMock()
        with pytest.raises(TypeError) as exc_info:
            UniswapV2Pool(
                fake_handle,
                address="0x" + "0" * 40,
                token0=MagicMock(),
                token1=MagicMock(),
                factory="0x" + "1" * 40,
                fee_token0=Fraction(3, 1000),
                fee_token1=Fraction(3, 1000),
            )

        message = str(exc_info.value)
        assert "Bot.build_pool" in message
        assert "make_v2_pool" in message
