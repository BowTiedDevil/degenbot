"""Survey C7 (TD-enum-coverage, epic HUDOHI) — typed coverage at the FFI seam.

``ConcentratedLiquidityView.coverage`` returns the registered pyclass enum
``degenbot._ffi.PoolTickCoverage`` — never the retired stringified form — and
the Python CL state reads that enum with no ``coverage == "sparse"`` probe.
The V3/V4 ``sparse_liquidity_map`` properties share one implementation via the
``ConcentratedLiquidityPoolState`` mixin (survey C4).
"""

from __future__ import annotations

from degenbot._ffi import Bot, PoolTickCoverage
from degenbot.abi import encode as abi_encode
from degenbot.crypto import keccak256
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v3_pool_factory import make_v3_pool
from tests.helpers.v4_pool_factory import make_v4_pool
from tests.pool_companion._helpers import (
    UNISWAP_V3_FACTORY,
    USDC_WETH_V3_POOL,
    V3_FEE,
    V3_TICK_SPACING,
    make_usdc,
    make_weth,
)


def _compute_v4_pool_id(currency0: str, currency1: str, fee: int, tick_spacing: int) -> str:
    """Mirror UniswapV4Pool's pool_id derivation so the test pool validates."""
    hooks = "0x" + "00" * 20
    return (
        "0x"
        + keccak256(
            abi_encode(
                types=["address", "address", "uint24", "int24", "address"],
                args=[currency0, currency1, fee, tick_spacing, hooks],
            )
        ).hex()
    )


# 1:1 price at tick 0, non-zero in-range liquidity.
_SQRT_PRICE_1TO1 = 79228162514264337593543950336
_LIQUIDITY = 1_000_000


def _make_v3(py_bot: Bot, *, tick_data: dict[int, tuple[int, int, int]] | None):
    return make_v3_pool(
        USDC_WETH_V3_POOL,
        token0=make_weth(),
        token1=make_usdc(),
        factory=UNISWAP_V3_FACTORY,
        fee=V3_FEE,
        tick_spacing=V3_TICK_SPACING,
        sqrt_price_x96=_SQRT_PRICE_1TO1,
        tick=0,
        liquidity=_LIQUIDITY,
        tick_data=tick_data,
        py_bot=py_bot,
    )


def test_coverage_is_the_typed_enum_sparse() -> None:
    """An empty tick map registers Sparse and the view returns the enum."""
    pool = _make_v3(Bot(), tick_data=None)

    view = pool._py_pool.concentrated_liquidity()
    assert view.coverage == PoolTickCoverage.Sparse
    assert isinstance(view.coverage, PoolTickCoverage)
    assert pool.sparse_liquidity_map is True


def test_coverage_is_the_typed_enum_tracked() -> None:
    """A seeded tick map registers Tracked and the view returns the enum."""
    pool = _make_v3(Bot(), tick_data={0: (1000, 500, 0)})

    view = pool._py_pool.concentrated_liquidity()
    assert view.coverage == PoolTickCoverage.Tracked
    assert isinstance(view.coverage, PoolTickCoverage)
    assert pool.sparse_liquidity_map is False


def test_coverage_enum_is_hashable_and_memberwise_comparable() -> None:
    """The pyclass enum is a value type (eq + hash), like ``PoolKind``."""
    assert PoolTickCoverage.Sparse != PoolTickCoverage.Tracked
    assert {PoolTickCoverage.Sparse, PoolTickCoverage.Sparse} == {PoolTickCoverage.Sparse}


def test_v4_sparse_liquidity_map_reads_the_enum() -> None:
    """The V4 twin (collapsed onto the shared mixin) reads the same enum."""
    bot = Bot()
    token0 = make_erc20(bot, address=f"0x{'c' * 40}", name="C0", symbol="C0", decimals=18)
    token1 = make_erc20(bot, address=f"0x{'d' * 40}", name="D1", symbol="D1", decimals=18)

    pool = make_v4_pool(
        pool_id=_compute_v4_pool_id(token0.address, token1.address, V3_FEE, V3_TICK_SPACING),
        pool_manager_address=f"0x{'e' * 40}",
        token0=token0,
        token1=token1,
        fee=V3_FEE,
        tick_spacing=V3_TICK_SPACING,
        hook_address=None,
        sqrt_price_x96=_SQRT_PRICE_1TO1,
        tick=0,
        liquidity=_LIQUIDITY,
        tick_data=None,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=V3_FEE,
        state_block=0,
        py_bot=bot,
        coverage="sparse",
    )

    assert pool._py_pool.concentrated_liquidity().coverage == PoolTickCoverage.Sparse
    assert pool.sparse_liquidity_map is True
