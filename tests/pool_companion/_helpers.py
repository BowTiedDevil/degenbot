"""Shared constants + builders for the collapsed companion guard/io-free suite.

The per-class ``io_free`` / ``construction_guard`` files were collapsed into
this package (see the task report for the class -> case mapping). This module
holds the pieces every module shares: the reference mainnet pool addresses,
the token/pool builders, and the one-block ``OfflineProvider`` casettes used by
``Bot.build_*`` choreography tests.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot._ffi import Bot as _Engine
from degenbot.abi import encode as abi_encode
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.constants import ZERO_ADDRESS
from degenbot.provider import OfflineProvider
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.utils.bytes import to_bytes
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI
from tests.helpers.erc20_factory import make_erc20

if TYPE_CHECKING:
    import pathlib

    from degenbot.erc20.erc20 import Erc20Token

# A single shared handle-less engine for token companions (metadata-only, no I/O).
PY_BOT = _Engine()

WETH_ADDR = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
USDC_ADDR = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"

# V2 reference pool (WETH/USDC pair).
WETH_USDC_V2_POOL = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
UNISWAP_V2_FACTORY = "0x5C69bEe701ef814E44274f655e7632cB715C14B6"

# V3 reference pool (USDC/WETH 0.3%).
USDC_WETH_V3_POOL = "0x8ad599c3a0ff1de082011efddc58f1908eb6e6d8"
UNISWAP_V3_FACTORY = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
V3_FEE = 3000
V3_TICK_SPACING = 60

# V4 reference pool (ETH/USDC).
V4_POOL_MANAGER = "0x000000000004444c5dc75cB358380D2e3dE08A90"
V4_STATE_VIEW = "0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227"
V4_POOL_ID = "0x21c67e77068de97969ba93d4aab21826d33ca12bb9f565d8496e8fda8a82ca27"
V4_FEE = 500
V4_TICK_SPACING = 10
V4_HOOKS = ZERO_ADDRESS


def make_test_config(tmp_path: pathlib.Path) -> DegenbotConfig:
    """A throwaway config bound to ``tmp_path`` and the archive-node RPC URI."""
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: ETHEREUM_ARCHIVE_NODE_HTTP_URI},
        default_chain_id=1,
    )


def make_weth(py_bot: _Engine = PY_BOT) -> Erc20Token:
    return make_erc20(
        py_bot, WETH_ADDR, chain_id=1, name="Wrapped Ether", symbol="WETH", decimals=18
    )


def make_usdc(py_bot: _Engine = PY_BOT) -> Erc20Token:
    return make_erc20(py_bot, USDC_ADDR, chain_id=1, name="USD Coin", symbol="USDC", decimals=6)


def make_native_eth(py_bot: _Engine = PY_BOT) -> Erc20Token:
    return make_erc20(py_bot, ZERO_ADDRESS, chain_id=1, name="Ether", symbol="ETH", decimals=18)


def v2_offline_provider(
    *,
    weth_addr: str,
    usdc_addr: str,
    factory_addr: str,
    pool_addr: str,
    block: int = 18_000_000,
) -> OfflineProvider:
    """A one-block ``OfflineProvider`` serving the V2 build RPC responses.

    The Rust ``PoolBuilder`` choreography is alloy-only, so the pool build +
    type-resolution probing must be served from recorded offline data. The
    recorded calls mirror the alloy transport's exact-calldata keying
    (``{addr}:0x{data}``): ``factory()``/``token0()``/``token1()``/
    ``getReserves()`` for the pool (with ``slot0()`` recorded as a redirect so
    ``probe_pool_type`` resolves V2, not V3).
    """
    factory_enc = abi_encode(types=["address"], args=[factory_addr]).hex()
    token0_enc = abi_encode(types=["address"], args=[weth_addr]).hex()
    token1_enc = abi_encode(types=["address"], args=[usdc_addr]).hex()
    reserves_enc = abi_encode(
        types=["uint112", "uint112", "uint32"], args=[1000 * 10**18, 2_000_000 * 10**6, 0]
    ).hex()
    to = pool_addr.lower()
    calls = {
        f"{to}:0x0902f1ac": reserves_enc,  # getReserves()
        f"{to}:0xc45a0155": factory_enc,  # factory()
        f"{to}:0x0dfe1681": token0_enc,  # token0()
        f"{to}:0xd21220a7": token1_enc,  # token1()
        f"{to}:0x3850c7bd": None,  # slot0() reverts on a V2 pool
    }
    return OfflineProvider(
        chain_id=1,
        blocks={str(block): {"timestamp": 1_700_000_000, "calls": calls, "code": {}}},
    )


def v3_offline_provider(
    *,
    weth_addr: str,
    usdc_addr: str,
    factory_addr: str,
    pool_addr: str,
    sqrt_price: int,
    tick: int,
    liquidity: int,
    block: int = 18_000_000,
) -> OfflineProvider:
    """A one-block ``OfflineProvider`` serving the V3 build RPC responses.

    The recorded calls key on the alloy transport's exact-calldata format:
    ``factory()``/``token0()``/``token1()``/``fee()``/``tickSpacing()``/
    ``slot0()``/``liquidity()`` plus a ``tickBitmap(int16)`` read at the single
    sparse seed word (empty -> no ``ticks()`` calls). The seed word mirrors the
    core's ``get_tick_word_and_bit_position`` (tick / spacing, then ``>> 8``).
    """
    factory_enc = abi_encode(types=["address"], args=[factory_addr]).hex()
    token0_enc = abi_encode(types=["address"], args=[weth_addr]).hex()
    token1_enc = abi_encode(types=["address"], args=[usdc_addr]).hex()
    fee_enc = abi_encode(types=["uint24"], args=[V3_FEE]).hex()
    spacing_enc = abi_encode(types=["int24"], args=[V3_TICK_SPACING]).hex()
    slot0_enc = abi_encode(
        types=["uint160", "int24", "uint16", "uint16", "uint16", "uint8", "bool"],
        args=[sqrt_price, tick, 0, 0, 0, 0, False],
    ).hex()
    liquidity_enc = abi_encode(types=["uint128"], args=[liquidity]).hex()
    to = pool_addr.lower()
    compressed = tick // V3_TICK_SPACING
    word = compressed >> 8
    tick_bitmap_arg = word.to_bytes(32, "big", signed=True).hex()
    calls = {
        f"{to}:0xc45a0155": factory_enc,  # factory()
        f"{to}:0x0dfe1681": token0_enc,  # token0()
        f"{to}:0xd21220a7": token1_enc,  # token1()
        f"{to}:0xddca3f43": fee_enc,  # fee()
        f"{to}:0xd0c93a7c": spacing_enc,  # tickSpacing()
        f"{to}:0x3850c7bd": slot0_enc,  # slot0()
        f"{to}:0x1a686502": liquidity_enc,  # liquidity()
        f"{to}:0x5339c296{tick_bitmap_arg}": abi_encode(
            types=["uint256"], args=[0]
        ).hex(),  # tickBitmap(current_word) -> empty
    }
    return OfflineProvider(
        chain_id=1,
        blocks={str(block): {"timestamp": 1_700_000_000, "calls": calls, "code": {}}},
    )


def v4_offline_provider(
    *,
    pool_manager: str,
    state_view: str,
    pool_id: str,
    sqrt_price: int,
    tick: int,
    protocol_fee: int,
    lp_fee: int,
    liquidity: int,
    block: int = 18_000_000,
) -> OfflineProvider:
    """A one-block ``OfflineProvider`` serving the V4 managed-pool build responses.

    All calls hit the ``StateView`` contract (``PoolManager`` itself does NOT
    expose the state getters): ``getSlot0(bytes32)`` + ``getLiquidity(bytes32)``
    + a ``getTickBitmap(bytes32,int16)`` sparse seed read. The seed word mirrors
    the core's ``get_tick_word_and_bit_position``.
    """
    pid = to_bytes(pool_id)
    slot0_calldata = encode_function_calldata("getSlot0(bytes32)", [pid])
    liquidity_calldata = encode_function_calldata("getLiquidity(bytes32)", [pid])
    compressed = tick // V4_TICK_SPACING
    word = compressed >> 8
    tick_bitmap_calldata = encode_function_calldata("getTickBitmap(bytes32,int16)", [pid, word])
    slot0_encoded = abi_encode(
        types=["uint160", "int24", "uint24", "uint24"],
        args=[sqrt_price, tick, protocol_fee, lp_fee],
    )
    sv = state_view.lower()
    calls = {
        f"{sv}:0x{slot0_calldata.hex()}": slot0_encoded.hex(),
        f"{sv}:0x{liquidity_calldata.hex()}": abi_encode(types=["uint256"], args=[liquidity]).hex(),
        f"{sv}:0x{tick_bitmap_calldata.hex()}": abi_encode(types=["uint256"], args=[0]).hex(),
    }
    return OfflineProvider(
        chain_id=1,
        blocks={str(block): {"timestamp": 1_700_000_000, "calls": calls, "code": {}}},
    )
