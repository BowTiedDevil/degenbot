"""UniswapV2Pool._from_py_pool is the Polars-style slim seam (ADR-005 / OGTTCS).

``_from_py_pool(cls, py_pool) -> Self`` takes ONLY the Rust handle. Every
identity field is recovered off the handle — the Polars ``_from_pydf`` end
state for V2. This test asserts delegation: the companion reads identity from
the handle rather than constructor args, and the recovered values match what
was registered.
"""

import inspect
from fractions import Fraction

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import PyBot
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool
from tests.helpers.v3_pool_factory import make_v3_pool


class TestFromPyPoolSlimSeam:
    """``_from_py_pool`` takes only ``py_pool`` and reads identity off the handle."""

    def test_signature_takes_only_py_pool(self) -> None:
        """The classmethod accepts a single positional arg (the handle)."""
        sig = inspect.signature(UniswapV2Pool._from_py_pool)
        params = [p for p in sig.parameters.values() if p.name != "cls"]
        assert len(params) == 1, (
            f"_from_py_pool must take only py_pool, got params: {[p.name for p in params]}"
        )
        assert params[0].name == "py_pool"

    def test_recovered_identity_matches_registration(self) -> None:
        """Address, factory, fees, tokens, chain_id all read off the handle."""
        py_bot = PyBot()

        token0 = make_erc20(
            py_bot=py_bot,
            address="0x" + "1" * 40,
            name="Token0",
            symbol="T0",
            decimals=18,
            chain_id=1,
        )
        token1 = make_erc20(
            py_bot=py_bot,
            address="0x" + "2" * 40,
            name="Token1",
            symbol="T1",
            decimals=18,
            chain_id=1,
        )

        pool = make_v2_pool(
            address="0x" + "a" * 40,
            token0=token0,
            token1=token1,
            factory="0x" + "f" * 40,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1_000_000,
            reserves_token1=2_000_000,
            chain_id=1,
            py_bot=py_bot,
        )

        # Identity recovered from the handle, not constructor args.
        assert pool.address == get_checksum_address("0x" + "a" * 40)
        assert pool.factory == get_checksum_address("0x" + "f" * 40)
        assert pool.fee_token0 == Fraction(3, 1000)
        assert pool.fee_token1 == Fraction(3, 1000)
        assert pool.token0.address == token0.address
        assert pool.token1.address == token1.address

    def test_rejects_non_v2_handle_with_family_error(self) -> None:
        """A V3-family handle must be rejected with a family-mismatch error.

        ``_from_py_pool`` is the V2 companion seam; wrapping a handle whose
        ``PoolEntry`` is not ``V2`` would read empty/default identity (the
        union-handle leak) and later crash with a confusing
        ``ZeroDivisionError`` on ``Fraction(denom - gamma, denom)``. The
        ``pool_family`` assertion fails fast with a clear message instead.
        """
        py_bot = PyBot()

        token0 = make_erc20(
            py_bot=py_bot,
            address="0x" + "1" * 40,
            name="Token0",
            symbol="T0",
            decimals=18,
            chain_id=1,
        )
        token1 = make_erc20(
            py_bot=py_bot,
            address="0x" + "2" * 40,
            name="Token1",
            symbol="T1",
            decimals=18,
            chain_id=1,
        )

        v3_pool = make_v3_pool(
            address="0x" + "b" * 40,
            token0=token0,
            token1=token1,
            factory="0x" + "f" * 40,
            fee=3000,
            tick_spacing=60,
            sqrt_price_x96=1 << 96,
            tick=0,
            liquidity=1_000_000,
            state_block=18_000_000,
            py_bot=py_bot,
        )

        # ``_py_pool`` is the shared ``PyLiquidityPool`` handle type; the V3
        # companion holds the same handle shape, so passing it to the V2 seam
        # is the misuse the ``pool_family`` assertion must catch.
        with pytest.raises(DegenbotValueError, match="V2-family"):
            UniswapV2Pool._from_py_pool(v3_pool._py_pool)  # noqa: SLF001

    def test_tokens_recovered_from_same_bot(self) -> None:
        """token0/token1 companions are rebuilt off the pool's handle."""
        py_bot = PyBot()

        token0 = make_erc20(
            py_bot=py_bot,
            address="0x" + "3" * 40,
            name="A",
            symbol="A",
            decimals=18,
            chain_id=1,
        )
        token1 = make_erc20(
            py_bot=py_bot,
            address="0x" + "4" * 40,
            name="B",
            symbol="B",
            decimals=18,
            chain_id=1,
        )

        pool = make_v2_pool(
            address="0x" + "b" * 40,
            token0=token0,
            token1=token1,
            factory="0x" + "f" * 40,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1_000,
            reserves_token1=2_000,
            chain_id=1,
            py_bot=py_bot,
        )

        # The companions are rebuilt off the pool's handle, so they read from
        # the same shared BotState as the pool (ADR-006).
        assert isinstance(pool.token0, Erc20Token)
        assert isinstance(pool.token1, Erc20Token)
        assert pool.token0.address == token0.address
        assert pool.token1.address == token1.address

    def test_cross_bot_token_recovery_fix(self) -> None:
        """Tokens built against a DIFFERENT PyBot are re-registered in the pool's Bot.

        The D1 invariant (ADR-006): the test factory's cross-Bot convenience is
        fixed, not preserved. Tokens passed in from a separate ``PyBot`` get
        re-registered in the pool's ``PyBot`` so the handle is self-describing.
        """
        other_bot = PyBot()  # tokens live here
        pool_bot = PyBot()  # pool lives here

        token0 = make_erc20(
            py_bot=other_bot,
            address="0x" + "5" * 40,
            name="X",
            symbol="X",
            decimals=6,
            chain_id=1,
        )
        token1 = make_erc20(
            py_bot=other_bot,
            address="0x" + "6" * 40,
            name="Y",
            symbol="Y",
            decimals=18,
            chain_id=1,
        )

        pool = make_v2_pool(
            address="0x" + "c" * 40,
            token0=token0,
            token1=token1,
            factory="0x" + "f" * 40,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1_000,
            reserves_token1=2_000,
            chain_id=1,
            py_bot=pool_bot,  # different Bot than the tokens
        )

        # The pool's tokens were re-registered in pool_bot, so the handle
        # resolves them despite the originals living in other_bot.
        assert pool.token0.address == token0.address
        assert pool.token1.address == token1.address
        # Decimals recovered from the re-registered handle, not the original.
        assert pool.token0.decimals == 6
        assert pool.token1.decimals == 18
