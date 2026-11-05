"""The offline two-hop V2 cycle engine the executor seams are built over.

One construction, one home: the batch-executor seam tests and the
session-consumption oracle both need a REAL ``ArbitrageEngine`` with a
registered 2-hop path (a payload row's resolve miss is the loud-abort arm, so
the registered path must be real). The cycle is a WETH→USDC→WETH loop whose
``path_info_for`` projection is a 2-hop V2 ``PathInfo`` with ``fee=30``.

No RPC, no DB: ``Bot`` + ``make_erc20`` + ``make_v2_pool`` build the state in
process. The constants mirror ``tests/arbitrage/test_synthetic_v2_round_trip.py``
(the proven offline 2-hop V2 topology) so the path registers and eager-solves.
"""

from __future__ import annotations

from fractions import Fraction

from degenbot.arbitrage.engine_registry import ArbitrageEngine, EngineRegistry
from degenbot.constants import ZERO_ADDRESS
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.seam import Bot
from tests.helpers.v2_pool_factory import make_v2_pool

_WETH = "0xC02aaA39b223FE8D0A0e5C4f27eAD9083C756Cc2"
_USDC = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
_POOL_A = "0x1100000000000000000000000000000000000000"  # WETH<->USDC
_POOL_B = "0x1200000000000000000000000000000000000000"  # USDC<->WETH


def build_two_hop_v2_engine() -> tuple[ArbitrageEngine, int]:
    """Build an engine + one registered 2-hop V2 path (0.3% fee cycle).

    Returns:
        The engine and the registered path's id. The path is
        ``[(pool_a, True), (pool_b, False)]`` — ``path_info_for`` projects a
        2-hop ``PathInfo`` (both V2, ``fee=30``).

    """
    py_bot = Bot()
    weth = make_erc20(py_bot, _WETH, chain_id=1, name="Wrapped Ether", symbol="WETH", decimals=18)
    usdc = make_erc20(py_bot, _USDC, chain_id=1, name="USD Coin", symbol="USDC", decimals=6)
    pool_a = make_v2_pool(
        address=_POOL_A,
        token0=weth,
        token1=usdc,
        factory=ZERO_ADDRESS,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=800 * 10**18,
        reserves_token1=1_600_000 * 10**6,
        state_block=18_000_000,
        py_bot=py_bot,
    )
    pool_b = make_v2_pool(
        address=_POOL_B,
        token0=usdc,
        token1=weth,
        factory=ZERO_ADDRESS,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=1_500_000 * 10**6,
        reserves_token1=800 * 10**18,
        state_block=18_000_000,
        py_bot=py_bot,
    )
    registry = EngineRegistry(bot=None, engine=ArbitrageEngine(py_bot=py_bot))
    registry.register_v2_pool(pool_a)
    registry.register_v2_pool(pool_b)
    path_id, _created = registry.register_path([(pool_a, True), (pool_b, False)])
    return registry.engine, path_id
