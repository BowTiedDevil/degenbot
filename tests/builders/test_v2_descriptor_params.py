"""FMO2GE: make_v2_pool + builders pass descriptor params into register_v2_pool.

``register_v2_pool`` gained ``variant`` / ``stable_swap`` / ``fee_denominator``
params (Rust commit e80f7574). The builder + test factory must thread these
through so the Rust-side ``V2PoolDescriptor`` carries the DEX identity + Camelot
stable strategy, replacing the old post-construction companion mutation.
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING

from degenbot._ffi.dex_identity import dex_identity
from degenbot._ffi import Bot
from degenbot.types import LiquidityPool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

if TYPE_CHECKING:
    from degenbot.erc20.erc20 import Erc20Token

_PY_BOT = Bot()


def _make_token(addr: str, *, symbol: str, decimals: int) -> Erc20Token:
    return make_erc20(_PY_BOT, addr, name=symbol, symbol=symbol, decimals=decimals)


class _RegisterSpy:
    """Wraps a Bot, capturing register_v2_pool kwargs before delegating."""

    def __init__(self, py_bot: Bot) -> None:
        self._py_bot = py_bot
        self.captured_kwargs: dict | None = None

    def register_v2_pool(self, **kwargs: object) -> int:
        self.captured_kwargs = dict(kwargs)
        return self._py_bot.register_v2_pool(**kwargs)

    def get_token(self, address: str) -> object:
        return self._py_bot.get_token(address)

    def register_token(
        self, address: str, name: str, symbol: str, decimals: int, chain_id: int
    ) -> object:
        return self._py_bot.register_token(address, name, symbol, decimals, chain_id)

    def get_pool(self, pool_id: int) -> LiquidityPool | None:
        return self._py_bot.get_pool(pool_id)


class TestMakeV2PoolPassesDescriptorParams:
    """``make_v2_pool`` threads variant/stable_swap/fee_denominator into Rust."""

    def test_uniswap_default_variant_passed(self) -> None:
        """Without dex, register_v2_pool receives variant='uniswap-v2'."""
        token0 = _make_token("0x" + "1a" * 20, symbol="T0", decimals=18)
        token1 = _make_token("0x" + "1b" * 20, symbol="T1", decimals=18)
        spy = _RegisterSpy(Bot())

        make_v2_pool(
            "0x" + "2c" * 20,
            token0=token0,
            token1=token1,
            factory="0x" + "ab" * 20,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1_000_000,
            reserves_token1=2_000_000,
            py_bot=spy,
        )

        assert spy.captured_kwargs is not None
        assert spy.captured_kwargs["variant"] == "uniswap-v2"
        assert spy.captured_kwargs["stable_swap"] is False
        assert spy.captured_kwargs["fee_denominator"] is None

    def test_variant_derived_from_dex_preset(self) -> None:
        """With dex, variant comes from dex.variant."""
        token0 = _make_token("0x" + "3a" * 20, symbol="T0", decimals=18)
        token1 = _make_token("0x" + "3b" * 20, symbol="T1", decimals=18)
        spy = _RegisterSpy(Bot())
        dex = dex_identity("sushiswap-v2")
        assert dex is not None

        make_v2_pool(
            "0x" + "4c" * 20,
            token0=token0,
            token1=token1,
            reserves_token0=1_000_000,
            reserves_token1=2_000_000,
            dex=dex,
            py_bot=spy,
        )

        assert spy.captured_kwargs is not None
        assert spy.captured_kwargs["variant"] == "sushiswap-v2"

    def test_explicit_stable_swap_fee_denominator_passed(self) -> None:
        """Explicit stable_swap + fee_denominator are forwarded."""
        token0 = _make_token("0x" + "5a" * 20, symbol="T0", decimals=18)
        token1 = _make_token("0x" + "5b" * 20, symbol="T1", decimals=18)
        spy = _RegisterSpy(Bot())

        make_v2_pool(
            "0x" + "6c" * 20,
            token0=token0,
            token1=token1,
            factory="0x" + "cd" * 20,
            fee_token0=Fraction(5, 1000),
            fee_token1=Fraction(7, 1000),
            reserves_token0=5_000_000,
            reserves_token1=8_000_000,
            stable_swap=True,
            fee_denominator=1000,
            py_bot=spy,
        )

        assert spy.captured_kwargs is not None
        assert spy.captured_kwargs["stable_swap"] is True
        assert spy.captured_kwargs["fee_denominator"] == 1000
