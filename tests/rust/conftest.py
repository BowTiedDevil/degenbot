from __future__ import annotations

import random
from fractions import Fraction
from typing import TYPE_CHECKING

import pytest

from degenbot._ffi import Bot, to_checksum_address
from degenbot.arbitrage.engine_registry import ArbitrageEngine, EngineRegistry
from degenbot.constants import ZERO_ADDRESS
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

if TYPE_CHECKING:
    from degenbot._ffi import ChecksummedAddress


@pytest.fixture(scope="module")
def random_addresses() -> list[bytes]:
    return [random.getrandbits(160).to_bytes(20, byteorder="big") for _ in range(10_000)]


@pytest.fixture(scope="module")
def checksummed_random_addresses(random_addresses) -> list[ChecksummedAddress]:
    return [to_checksum_address(addr) for addr in random_addresses]


# NXM2BF: `DispatchCandidate.__new__` resolves its `composers::PathInfo` from
# a registered `path_id` via `PyArbitrageEngine::path_info_for_core` (no Python
# `PathInfo` dataclass round-trip). Seam tests must hand the candidate a real
# engine + a registered path_id. The engine's `register_and_solve_path`
# requires a ≥2-hop cycle, so this fixture builds a 2-hop V2 WETH→USDC→WETH
# cycle offline (Bot + make_erc20 + make_v2_pool, no RPC / no DB).
#
# Constants chosen to mirror `tests/arbitrage/test_synthetic_v2_round_trip.py`
# (the proven offline 2-hop V2 cycle topology) so the path registers +
# eager-solves. The two pools use a 0.3% fee (gamma=997, denom=1000) so the
# projected `V2HopInfo.fee == 30` (bips-of-10000), matching the Python
# `build_hops_from_pools` value the projection replaces.
_NXM2BF_WETH = "0xC02aaA39b223FE8D0A0e5C4f27eAD9083C756Cc2"
_NXM2BF_USDC = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
_NXM2BF_POOL_A = "0x1100000000000000000000000000000000000000"  # WETH↔USDC
_NXM2BF_POOL_B = "0x1200000000000000000000000000000000000000"  # USDC↔WETH


@pytest.fixture
def nxm2bf_v2_engine_and_path() -> tuple[ArbitrageEngine, int]:
    """A `PyArbitrageEngine` + a registered 2-hop V2 path (0.3% fee cycle).

    The path is `[(pool_a, True), (pool_b, False)]` — a WETH→USDC→WETH cycle.
    `path_info_for_core(path_id)` projects a 2-hop `PathInfo` (both V2,
    `fee=30`, `zfo=(True, False)`).
    """
    py_bot = Bot()
    weth = make_erc20(
        py_bot, _NXM2BF_WETH, chain_id=1, name="Wrapped Ether", symbol="WETH", decimals=18
    )
    usdc = make_erc20(py_bot, _NXM2BF_USDC, chain_id=1, name="USD Coin", symbol="USDC", decimals=6)
    pool_a = make_v2_pool(
        address=_NXM2BF_POOL_A,
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
        address=_NXM2BF_POOL_B,
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
    path_id = registry.register_path([(pool_a, True), (pool_b, False)])
    return registry.engine, path_id
