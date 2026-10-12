"""Derive -> build -> register: the by-tokens construction path (offline).

The pool-tracker family was removed (P1 tracker-family removal). The surviving
contract is the end-to-end flow the trackers wrapped: derive the CREATE2 pool
address from the token pair (``generate_v2_pool_address`` /
``generate_v3_pool_address``), build through the one supported door
(``Bot.build_pool``), and observe the registration in ``Bot.pools``.

The derivation math itself is pinned by
``tests/ffi/test_ffi_create2_exposure.py`` and the aerodrome clone-parity tests;
this module pins the composition. Fully offline: the V2/V3 build choreography is
served from recorded ``OfflineProvider`` cassettes (see
``tests/pool_companion/_helpers``), so the module runs in the default (no fork)
selection.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.uniswap import resolve_deployer, resolve_v2_init_hash, resolve_v3_init_hash
from degenbot.uniswap.v2_functions import generate_v2_pool_address
from degenbot.uniswap.v3_functions import generate_v3_pool_address
from tests.helpers.erc20_factory import make_erc20
from tests.pool_companion._helpers import (
    make_test_database,
    v2_offline_provider,
    v3_offline_provider,
)

if TYPE_CHECKING:
    import pathlib


MAINNET_UNISWAP_V2_FACTORY = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
MAINNET_UNISWAP_V3_FACTORY = get_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984")

MAINNET_WETH = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
MAINNET_USDC = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")

MAINNET_UNISWAPV2_WETH_USDC = get_checksum_address("0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc")
MAINNET_UNISWAPV3_WETH_USDC = get_checksum_address("0x8ad599c3a0ff1de082011efddc58f1908eb6e6d8")

V3_FEE = 3000
_SQRT_PRICE_X96 = 2_198_666_895_605_149_686_863
_TICK = -76_020
_LIQUIDITY = 1_234_567_890


def _seed_tokens(bot: Bot) -> None:
    """Register the WETH/USDC token companions so the offline build skips metadata RPC."""
    for address, name, symbol, decimals in (
        (MAINNET_WETH, "Wrapped Ether", "WETH", 18),
        (MAINNET_USDC, "USD Coin", "USDC", 6),
    ):
        bot.tokens.add(
            token_address=address,
            chain_id=1,
            token=make_erc20(
                bot._py_bot, address, chain_id=1, name=name, symbol=symbol, decimals=decimals
            ),
        )


def _v2_bot(tmp_path: pathlib.Path) -> Bot:
    bot = Bot(
        database=make_test_database(tmp_path),
        chain_id=1,
        provider=v2_offline_provider(
            weth_addr=MAINNET_WETH,
            usdc_addr=MAINNET_USDC,
            factory_addr=MAINNET_UNISWAP_V2_FACTORY,
            pool_addr=MAINNET_UNISWAPV2_WETH_USDC,
        ),
    )
    _seed_tokens(bot)
    return bot


def _derive_v2_pool_address() -> str:
    deployer = resolve_deployer(1, MAINNET_UNISWAP_V2_FACTORY)
    init_hash = resolve_v2_init_hash(1, MAINNET_UNISWAP_V2_FACTORY)
    return generate_v2_pool_address(
        deployer_address=deployer,
        token_addresses=(MAINNET_WETH, MAINNET_USDC),
        init_hash=init_hash,
    )


def test_derive_and_build_v2_pool(tmp_path: pathlib.Path) -> None:
    """The V2 pair address derives from the token set, builds, and registers."""
    pool_address = _derive_v2_pool_address()
    assert pool_address == MAINNET_UNISWAPV2_WETH_USDC

    bot = _v2_bot(tmp_path)
    pool = bot.build_pool(pool_address)

    assert pool.address == MAINNET_UNISWAPV2_WETH_USDC
    assert pool.token0.address == MAINNET_WETH
    assert pool.token1.address == MAINNET_USDC
    assert bot.pools.get(pool_address=pool.address, chain_id=1) is pool
    # Idempotent: a second build returns the registered instance.
    assert bot.build_pool(pool_address) is pool


def test_derive_and_build_v3_pool(tmp_path: pathlib.Path) -> None:
    """The V3 pool address derives from the token set + fee, builds, and registers."""
    deployer = resolve_deployer(1, MAINNET_UNISWAP_V3_FACTORY)
    init_hash = resolve_v3_init_hash(1, MAINNET_UNISWAP_V3_FACTORY)
    pool_address = generate_v3_pool_address(
        deployer_address=deployer,
        token_addresses=sorted((MAINNET_WETH, MAINNET_USDC)),
        fee=V3_FEE,
        init_hash=init_hash,
    )
    assert pool_address == MAINNET_UNISWAPV3_WETH_USDC

    bot = Bot(
        database=make_test_database(tmp_path),
        chain_id=1,
        provider=v3_offline_provider(
            weth_addr=MAINNET_WETH,
            usdc_addr=MAINNET_USDC,
            factory_addr=MAINNET_UNISWAP_V3_FACTORY,
            pool_addr=MAINNET_UNISWAPV3_WETH_USDC,
            sqrt_price=_SQRT_PRICE_X96,
            tick=_TICK,
            liquidity=_LIQUIDITY,
        ),
    )
    _seed_tokens(bot)
    pool = bot.build_pool(pool_address)

    assert pool.address == MAINNET_UNISWAPV3_WETH_USDC
    assert pool.token0.address == MAINNET_WETH
    assert pool.token1.address == MAINNET_USDC
    assert bot.pools.get(pool_address=pool.address, chain_id=1) is pool


def test_pool_remove_and_recreate(tmp_path: pathlib.Path) -> None:
    """A removed pool re-derives and re-builds to a fresh object (ADR-007).

    The unregister half of the pool seam: after ``bot.pools.remove`` drops the
    Python wrapper (and unregisters the core pool), the same derived address
    builds a brand-new instance and re-registers it.
    """
    pool_address = _derive_v2_pool_address()

    bot = _v2_bot(tmp_path)
    first = bot.build_pool(pool_address)
    bot.pools.remove(pool_address=first.address, chain_id=1)
    assert bot.pools.get(pool_address=first.address, chain_id=1) is None

    second = bot.build_pool(pool_address)
    assert second is not first
    assert bot.pools.get(pool_address=second.address, chain_id=1) is second
