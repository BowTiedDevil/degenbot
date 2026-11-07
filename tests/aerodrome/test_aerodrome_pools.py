from collections.abc import Generator

import pydantic_core
import pytest

from degenbot.aerodrome.types import AerodromeV3PoolState
from degenbot.checksum_cache import get_checksum_address
from degenbot.fork import AnvilFork, ForkLaunchConfig
from tests.conftest import TEST_BASE_FULL_NODE_HTTP_URI, _require_live_node
from tests.helpers.bot_factory import make_bot_with_provider

WETH_CONTRACT_ADDRESS = get_checksum_address(
    "0x4200000000000000000000000000000000000006",
)
CBETH_CONTRACT_ADDRESS = get_checksum_address(
    "0x2Ae3F1Ec7F1F5012CFEab0185bfc7aa3cf0DEc22",
)

AERODROME_V2_FACTORY_ADDRESS = get_checksum_address(
    "0x420DD381b31aEf6683db6B902084cB0FFECe40Da",
)

AERODROME_V3_FACTORY_ADDRESS = get_checksum_address(
    "0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A",
)
AERODROME_V3_TBTC_USDBC_POOL_ADDRESS = get_checksum_address(
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


# This mark will be applied to ALL tests in this file.
pytestmark = pytest.mark.base


@pytest.fixture(scope="module")
def fork_base_full() -> Generator[AnvilFork, None, None]:
    """Module-scoped base fork (overriding the conftest per-test default).

    Every test in this module is chain-read-only — construction, quoter
    calls, and simulated swaps touch no chain state — and all run at the
    same tip block, so one fork serves the file. Each test still builds
    its own ``Bot``, so in-memory pool state cannot leak between them.

    """
    _require_live_node()
    fork = AnvilFork(
        fork_url=TEST_BASE_FULL_NODE_HTTP_URI,
        launch=ForkLaunchConfig(
            storage_caching=False,
            anvil_opts=["--accounts=0", "--optimism"],
        ),
    )
    yield fork
    fork.close()


def test_create_pool(fork_base_full: AnvilFork):

    bot = make_bot_with_provider(fork_base_full.provider)
    lp = bot.build_pool(AERODROME_V3_TBTC_USDBC_POOL_ADDRESS)
    assert lp.address == AERODROME_V3_TBTC_USDBC_POOL_ADDRESS
    assert lp.factory == AERODROME_V2_FACTORY_ADDRESS
    assert lp.deployer_address == AERODROME_V2_FACTORY_ADDRESS


def test_aerodrome_v3_state(fork_base_full: AnvilFork) -> None:

    bot = make_bot_with_provider(fork_base_full.provider)
    lp = bot.build_pool(AERODROME_V3_CBETH_WETH_POOL_ADDRESS)
    assert isinstance(lp.state, AerodromeV3PoolState), f"{type(lp.state)=}"
