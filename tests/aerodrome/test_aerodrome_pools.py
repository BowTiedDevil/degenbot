"""Aerodrome pool construction over recorded chain data.

Construction replays the real ``Bot.build_pool`` fetch seam against per-block
``OfflineProvider`` corpora under ``tests/fixtures/chain_data/8453``
(``construction_*``, machine-captured by
``scripts/record_construction_cassettes.py``), so the factory/deployer identity
and the V3 state type are asserted against recorded chain responses with no
RPC. The ``replay_makes_no_network_calls`` import arms the replay dial-block:
a socket dial during these tests is a defect.
"""

from __future__ import annotations

from pathlib import Path

import pydantic_core

from degenbot.aerodrome.types import AerodromeV3PoolState
from degenbot.checksum_cache import get_checksum_address
from degenbot.provider import OfflineProvider
from tests.golden.oracle import replay_makes_no_network_calls  # ruff: ignore[unused-import]
from tests.helpers.bot_factory import make_bot_with_provider

AERODROME_V2_FACTORY_ADDRESS = get_checksum_address(
    "0x420DD381b31aEf6683db6B902084cB0FFECe40Da",
)

AERODROME_V2_TBTC_USDBC_POOL_ADDRESS = get_checksum_address(
    "0x723AEf6543aecE026a15662Be4D3fb3424D502A9",
)

AERODROME_V3_CBETH_WETH_POOL_ADDRESS = get_checksum_address(
    "0x47cA96Ea59C13F72745928887f84C9F52C3D7348",
)

AERODROME_V3_QUOTER_ABI = pydantic_core.from_json(
    """
    [{"inputs":[{"internalType":"address","name":"_factory","type":"address"},{"internalType":"address","name":"_WETH9","type":"address"}],"stateMutability":"nonpayable","type":"constructor"},{"inputs":[],"name":"WETH9","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[],"name":"factory","outputs":[{"internalType":"address","name":"","type":"address"}],"stateMutability":"view","type":"function"},{"inputs":[{"internalType":"bytes","name":"path","type":"bytes"},{"internalType":"uint256","name":"amountIn","type":"uint256"}],"name":"quoteExactInput","outputs":[{"internalType":"uint256","name":"amountOut","type":"uint256"},{"internalType":"uint160[]","name":"sqrtPriceX96AfterList","type":"uint160[]"},{"internalType":"uint32[]","name":"initializedTicksCrossedList","type":"uint32[]"},{"internalType":"uint256","name":"gasEstimate","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"components":[{"internalType":"address","name":"tokenIn","type":"address"},{"internalType":"address","name":"tokenOut","type":"address"},{"internalType":"uint256","name":"amountIn","type":"uint256"},{"internalType":"int24","name":"tickSpacing","type":"int24"},{"internalType":"uint160","name":"sqrtPriceLimitX96","type":"uint160"}],"internalType":"struct IQuoterV2.QuoteExactInputSingleParams","name":"params","type":"tuple"}],"name":"quoteExactInputSingle","outputs":[{"internalType":"uint256","name":"amountOut","type":"uint256"},{"internalType":"uint160","name":"sqrtPriceX96After","type":"uint160"},{"internalType":"uint32","name":"initializedTicksCrossed","type":"uint32"},{"internalType":"uint256","name":"gasEstimate","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"bytes","name":"path","type":"bytes"},{"internalType":"uint256","name":"amountOut","type":"uint256"}],"name":"quoteExactOutput","outputs":[{"internalType":"uint256","name":"amountIn","type":"uint256"},{"internalType":"uint160[]","name":"sqrtPriceX96AfterList","type":"uint160[]"},{"internalType":"uint32[]","name":"initializedTicksCrossedList","type":"uint32[]"},{"internalType":"uint256","name":"gasEstimate","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"components":[{"internalType":"address","name":"tokenIn","type":"address"},{"internalType":"address","name":"tokenOut","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"},{"internalType":"int24","name":"tickSpacing","type":"int24"},{"internalType":"uint160","name":"sqrtPriceLimitX96","type":"uint160"}],"internalType":"struct IQuoterV2.QuoteExactOutputSingleParams","name":"params","type":"tuple"}],"name":"quoteExactOutputSingle","outputs":[{"internalType":"uint256","name":"amountIn","type":"uint256"},{"internalType":"uint160","name":"sqrtPriceX96After","type":"uint160"},{"internalType":"uint32","name":"initializedTicksCrossed","type":"uint32"},{"internalType":"uint256","name":"gasEstimate","type":"uint256"}],"stateMutability":"nonpayable","type":"function"},{"inputs":[{"internalType":"int256","name":"amount0Delta","type":"int256"},{"internalType":"int256","name":"amount1Delta","type":"int256"},{"internalType":"bytes","name":"path","type":"bytes"}],"name":"uniswapV3SwapCallback","outputs":[],"stateMutability":"view","type":"function"}]
    """
)


_CORPUS_DIR = Path(__file__).resolve().parents[1] / "fixtures" / "chain_data" / "8453"

_TBTC_USDBC_CORPUS = _CORPUS_DIR / "construction_aerodrome_v2_tbtc_usdbc_block46875151.json"
_CBETH_WETH_CORPUS = _CORPUS_DIR / "construction_aerodrome_v3_cbeth_weth_block46875151.json"


def test_create_pool() -> None:
    """The tBTC/USDbC pool builds over its corpus with the deployed V2 identity."""

    bot = make_bot_with_provider(OfflineProvider.from_json_file(_TBTC_USDBC_CORPUS))
    lp = bot.build_pool(AERODROME_V2_TBTC_USDBC_POOL_ADDRESS)
    assert lp.address == AERODROME_V2_TBTC_USDBC_POOL_ADDRESS
    assert lp.factory == AERODROME_V2_FACTORY_ADDRESS
    assert lp.deployer_address == AERODROME_V2_FACTORY_ADDRESS


def test_aerodrome_v3_state() -> None:
    """The cbETH/WETH slipstream pool's state is the Aerodrome V3 state type."""

    bot = make_bot_with_provider(OfflineProvider.from_json_file(_CBETH_WETH_CORPUS))
    lp = bot.build_pool(AERODROME_V3_CBETH_WETH_POOL_ADDRESS)
    assert isinstance(lp.state, AerodromeV3PoolState), f"{type(lp.state)=}"
