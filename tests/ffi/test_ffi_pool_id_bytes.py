"""Pool IDs cross the FFI as validated 32-byte ``bytes``, round-tripped.

The domain pool ID (Balancer V2 ``PoolId``, V4 ``PoolId``) is a 32-byte
value: the pool arms and the V4 identity getter answer exactly those bytes,
and hex survives only at text/JSON boundaries (registration intakes, JSON
rows). These tests pin the round-trip — the bytes registered are the bytes
read back through the handle and consumed by the companion — and the
refusals: a read on a family with no 32-byte pool ID is loud, and registered
data that disagrees with the pool key is refused, never fixed up.
"""

from __future__ import annotations

from fractions import Fraction

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.utils.bytes import to_bytes
from tests.helpers.balancer_pool_factory import (
    make_balancer_stable_pool,
    make_balancer_weighted_pool,
)
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.seam import Bot
from tests.helpers.v4_pool_factory import make_v4_pool

_VAULT = "0xBA12222222228d8Ba445958a75a0704d566BF2C8"
_V4_POOL_MANAGER = "0x000000000004444c5dc75cB358380D2e3dE08A90"
# A known-good mainnet identity (USDC/WETH, fee 500, tick spacing 10, no
# hooks) — the recorded pool-ID literal is the round-trip's source of truth,
# not a re-derivation of the same hash.
_V4_POOL_ID = "0x4f88f7c99022eace4740c6898f59ce6a2e798a1e64ce54589720b7153eb224a7"

_WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
_USDC = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"


def _erc20(bot: Bot, address: str, symbol: str, decimals: int):
    return make_erc20(
        bot,
        address,
        name=symbol,
        symbol=symbol,
        decimals=decimals,
        chain_id=1,
    )


def _weighted_pool(bot: Bot, pool_id: bytes):
    return make_balancer_weighted_pool(
        "0x" + "aa" * 20,
        pool_id=pool_id,
        vault=_VAULT,
        tokens=[_erc20(bot, "0x" + "10" * 20, "T0", 18), _erc20(bot, "0x" + "20" * 20, "T1", 6)],
        balances=[1_000_000, 1_000_000],
        fee=Fraction(0),
        weights=[10**17, 9 * 10**17],
    )


def test_weighted_pool_id_round_trips_32_bytes() -> None:
    """The registered 32 bytes come back identically through arm and companion."""
    pool_id = bytes.fromhex("11" * 20) + (2).to_bytes(2, "big") + bytes.fromhex("99" * 10)
    lp = _weighted_pool(Bot(), pool_id)

    assert type(lp.pool_id) is bytes
    assert len(lp.pool_id) == 32
    assert lp.pool_id == pool_id
    assert lp._py_pool.balancer_weighted().pool_id == pool_id
    # The specialization slices out of the byte shape, not a hex string.
    assert lp.pool_specialization == 2


def test_stable_pool_id_round_trips_32_bytes() -> None:
    """The registered 32 bytes come back identically; the address slices from them."""
    bot = Bot()
    pool_id = bytes.fromhex("22" * 20) + (1).to_bytes(2, "big") + bytes.fromhex("88" * 10)
    lp = make_balancer_stable_pool(
        "0x" + "bb" * 20,
        pool_id=pool_id,
        vault=_VAULT,
        tokens=[_erc20(bot, "0x" + "10" * 20, "T0", 18), _erc20(bot, "0x" + "20" * 20, "T1", 6)],
        balances=[1_000_000, 1_000_000],
        fee=Fraction(0),
        amp=100_000,
        scaling_factors=[10**18, 10**18],
    )

    assert type(lp.pool_id) is bytes
    assert len(lp.pool_id) == 32
    assert lp.pool_id == pool_id
    assert lp._py_pool.balancer_stable().pool_id == pool_id
    assert lp.pool_specialization == 1
    # Pool address = first 20 bytes of the pool_id.
    assert lp.address == get_checksum_address("0x" + pool_id[:20].hex())


def test_v4_pool_id_round_trips_32_bytes() -> None:
    """The registered 32 bytes come back identically through handle and companion."""
    bot = Bot()
    pool = make_v4_pool(
        pool_id=_V4_POOL_ID,
        pool_manager_address=_V4_POOL_MANAGER,
        token0=_erc20(bot, _USDC, "USDC", 6),
        token1=_erc20(bot, _WETH, "WETH", 18),
        fee=500,
        tick_spacing=10,
        hook_address=ZERO_ADDRESS,
        sqrt_price_x96=2_198_666_895_605_149_686_863,
        tick=-76020,
        liquidity=9876543210,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=500000,
        state_block=18_000_000,
    )

    expected = to_bytes(_V4_POOL_ID)
    assert type(pool.pool_id) is bytes
    assert len(pool.pool_id) == 32
    assert pool.pool_id == expected
    assert pool._py_pool.pool_id == expected


def test_pool_id_read_refuses_a_family_without_one() -> None:
    """A handle whose registered family carries no 32-byte pool ID refuses the read."""
    lp = _weighted_pool(Bot(), bytes.fromhex("33" * 32))

    with pytest.raises(ValueError, match="not a v4 pool"):
        _ = lp._py_pool.pool_id


def test_v4_refuses_registered_data_that_disagrees_with_the_pool_key() -> None:
    """The registered pool ID is authoritative — a mismatch is refused, never fixed up."""
    bot = Bot()
    with pytest.raises(AssertionError, match="does not match calculated"):
        make_v4_pool(
            pool_id="0x" + "11" * 32,
            pool_manager_address=_V4_POOL_MANAGER,
            token0=_erc20(bot, _USDC, "USDC", 6),
            token1=_erc20(bot, _WETH, "WETH", 18),
            fee=500,
            tick_spacing=10,
            hook_address=ZERO_ADDRESS,
            sqrt_price_x96=2_198_666_895_605_149_686_863,
            tick=-76020,
            liquidity=9876543210,
            protocol_fee_zero_for_one=0,
            protocol_fee_one_for_zero=0,
            lp_fee=500000,
            state_block=18_000_000,
        )
