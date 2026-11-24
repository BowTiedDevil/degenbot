"""``inspect_path`` hop projection: each hop's identity comes from the shared core.

The supported-family (V2) projection is pinned exactly: the address plus the
retained gamma numerator the pool handle speaks.

A Solidly hop (an Aerodrome V2 pool, a Camelot ``stable_swap`` V2 pool) cannot
reach an inspected path at all: ADR-107 makes the settlement registry
execution-only, so admission refuses the route before it is registered and the
Solidly arms of the projection are unreachable through a registered path.
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING

import pytest

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.arbitrage.engine_registry import ArbitrageEngine, EngineRegistry
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.exceptions import UnsupportedCapabilityKind, UnsupportedPathCapabilityError
from tests.helpers.aerodrome_pool_factory import make_aerodrome_v2_pool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.seam import Bot
from tests.helpers.v2_pool_factory import make_v2_pool

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token
    from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
TOKEN = "0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"  # ruff:ignore[hardcoded-password-string]
V2_POOL = "0x1100000000000000000000000000000000000000"
AERO_POOL = "0x1200000000000000000000000000000000000000"
CAMELOT_POOL = "0x1300000000000000000000000000000000000000"

# The 0.3% fee as a Python ``Fraction``; the core stores the retained 997/1000.
V2_FEE = Fraction(3, 1000)
AERO_FEE = Fraction(3, 1000)
CAMELOT_FEE_TOKEN0 = Fraction(3, 1000)
CAMELOT_FEE_TOKEN1 = Fraction(5, 1000)


def _pair() -> tuple[Bot, Erc20Token, Erc20Token]:
    """One shared ``Bot`` plus the WETH/TOKEN pair every pool below pairs."""
    py_bot = Bot()
    weth = make_erc20(py_bot, WETH, chain_id=1, name="Wrapped Ether", symbol="WETH", decimals=18)
    token = make_erc20(py_bot, TOKEN, chain_id=1, name="A", symbol="A", decimals=18)
    return py_bot, weth, token


def _volatile_v2(bot: Bot, weth: Erc20Token, token: Erc20Token) -> UniswapV2Pool:
    """The unchanged supported-family hop: a plain constant-product V2 pool."""
    return make_v2_pool(
        address=V2_POOL,
        token0=weth,
        token1=token,
        factory=ZERO_ADDRESS,
        fee_token0=V2_FEE,
        fee_token1=V2_FEE,
        reserves_token0=800 * 10**18,
        reserves_token1=1_600_000 * 10**18,
        state_block=18_000_000,
        py_bot=bot,
    )


def _inspect_hops(registry: EngineRegistry, hops: list[tuple[object, bool]]) -> list[dict]:
    """Register the path and return its projected hop dicts."""
    path_id, _created = registry.register_path(hops)
    inspected = registry.engine.inspect_path(path_id)
    assert inspected is not None, "a registered path must inspect"
    assert inspected["path_id"] == path_id
    return inspected["hops"]


def _registry(bot: Bot, *pools: UniswapV2Pool | AerodromeV2Pool) -> EngineRegistry:
    """An engine over the shared core, with each pool registered by identity."""
    registry = EngineRegistry(bot=None, engine=ArbitrageEngine(py_bot=bot))
    for pool in pools:
        if isinstance(pool, AerodromeV2Pool):
            registry.register_aerodrome_pool(pool)
        else:
            registry.register_v2_pool(pool)
    return registry


def test_inspect_path_hops_carry_their_registered_identities() -> None:
    """Each inspected hop reports the supported family's own identity.

    The V2 hop answers with its address and the retained gamma numerator the
    pool handle speaks (never the charged 3), and no V3/V4-only keys.
    """
    py_bot, weth, token = _pair()
    volatile = _volatile_v2(py_bot, weth, token)

    hops = _inspect_hops(_registry(py_bot, volatile), [(volatile, True), (volatile, False)])

    assert hops == [
        {
            "type": "v2",
            "address": get_checksum_address(V2_POOL),
            "zero_for_one": True,
            "fee": V2_FEE.denominator - V2_FEE.numerator,
        },
        {
            "type": "v2",
            "address": get_checksum_address(V2_POOL),
            "zero_for_one": False,
            "fee": V2_FEE.denominator - V2_FEE.numerator,
        },
    ]


def test_a_solidly_hop_is_refused_before_it_can_be_inspected() -> None:
    """Both Solidly registrations are refused at admission (ADR-107 D1).

    An Aerodrome stable pool and a Camelot ``stable_swap`` pool are one
    capability family with no settlement command composition, so the route is
    refused at registration and no path exists for the projection to inspect.
    """
    py_bot, weth, token = _pair()
    volatile = _volatile_v2(py_bot, weth, token)
    aero_pool = make_aerodrome_v2_pool(
        AERO_POOL,
        token0=token,
        token1=weth,
        factory=ZERO_ADDRESS,
        fee=AERO_FEE,
        stable=True,
        reserves_token0=1_600_000 * 10**18,
        reserves_token1=800 * 10**18,
        state_block=18_000_000,
        py_bot=py_bot,
    )
    camelot = make_v2_pool(
        address=CAMELOT_POOL,
        token0=token,
        token1=weth,
        factory=ZERO_ADDRESS,
        fee_token0=CAMELOT_FEE_TOKEN0,
        fee_token1=CAMELOT_FEE_TOKEN1,
        reserves_token0=1_600_000 * 10**18,
        reserves_token1=800 * 10**18,
        stable_swap=True,
        variant="camelot-v2-stable",
        state_block=18_000_000,
        py_bot=py_bot,
    )
    registry = _registry(py_bot, volatile, aero_pool, camelot)

    for stable_hop in (aero_pool, camelot):
        with pytest.raises(UnsupportedPathCapabilityError) as refusal_info:
            registry.register_path([(volatile, True), (stable_hop, True)])
        refusal = refusal_info.value
        assert refusal.kind == UnsupportedCapabilityKind.Family
        assert refusal.family == "solidly-stable"
        assert refusal.hop == 1
    assert registry.engine.path_count() == 0, "neither route was registered"
