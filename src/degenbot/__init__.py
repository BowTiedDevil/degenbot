"""degenbot: Ethereum DEX helper library."""

import importlib
import warnings

from .abi_adapter import (
    AbiAdapter,
    AbiBackend,
    AbiDecodeError,
    AbiEncodeError,
    AbiUnsupportedOperation,
    get_default_adapter,
    get_default_backend,
)
from .abi_adapter import decode as abi_decode
from .abi_adapter import decode_single as abi_decode_single
from .abi_adapter import encode as abi_encode
from .async_bot import AsyncBot
from .bot import Bot
from .checksum_cache import get_checksum_address
from .connection import (
    AsyncConnectionManager,
    ConnectionManager,
)
from .degenbot_rs import (
    decode_return_data,
    encode_function_call,
    get_function_selector,
    get_sqrt_ratio_at_tick,
    get_tick_at_sqrt_ratio,
    to_checksum_address,
)
from .version import __version__

# isort: split

from .aerodrome import (
    AerodromeV2Pool,
    AerodromeV2PoolState,
    AerodromeV2PoolTracker,
    AerodromeV3Pool,
    AerodromeV3PoolState,
    AerodromeV3PoolTracker,
)
from .anvil_fork import AnvilFork
from .arbitrage import (
    ApprovalStrategy,
    ArbitrageCalculationResult,
    EncodedCall,
    FlatComposer,
    NoApprovals,
    PayloadComposer,
    V4PoolKey,
    generate_payloads,
)
from .camelot import CamelotLiquidityPool
from .chainlink import ChainlinkPriceContract
from .curve import (
    CurveStableswapPool,
    CurveStableswapPoolSimulationResult,
    CurveStableswapPoolState,
    CurveStableSwapPoolStateUpdated,
)
from .erc20 import Erc20Token, EtherPlaceholder
from .logging import logger
from .pancakeswap import (
    PancakeswapV2Pool,
    PancakeswapV2PoolTracker,
    PancakeswapV3Pool,
    PancakeswapV3PoolTracker,
)
from .registry import (
    ManagedPoolRegistry,
    PoolRegistry,
    PoolTypeRegistry,
    TokenRegistry,
    pool_type_registry,
)
from .sushiswap import (
    SushiswapV2Pool,
    SushiswapV2PoolTracker,
    SushiswapV3Pool,
    SushiswapV3PoolTracker,
)
from .swapbased import SwapbasedV2Pool, SwapbasedV2PoolTracker
from .uniswap import (
    UniswapV2Pool,
    UniswapV2PoolExternalUpdate,
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
    UniswapV2PoolTracker,
    UniswapV3LiquiditySnapshot,
    UniswapV3Pool,
    UniswapV3PoolExternalUpdate,
    UniswapV3PoolSimulationResult,
    UniswapV3PoolState,
    UniswapV3PoolTracker,
    UniswapV4LiquiditySnapshot,
    UniswapV4Pool,
    UniswapV4PoolExternalUpdate,
    UniswapV4PoolState,
)

__all__ = (
    "AbiAdapter",
    "AbiBackend",
    "AbiDecodeError",
    "AbiEncodeError",
    "AbiUnsupportedOperation",
    "AerodromeV2Pool",
    "AerodromeV2PoolState",
    "AerodromeV2PoolTracker",
    "AerodromeV3Pool",
    "AerodromeV3PoolState",
    "AerodromeV3PoolTracker",
    "AnvilFork",
    "ApprovalStrategy",
    "ArbitrageCalculationResult",
    "AsyncBot",
    "AsyncConnectionManager",
    "Bot",
    "CamelotLiquidityPool",
    "ChainlinkPriceContract",
    "ConnectionManager",
    "CurveStableSwapPoolStateUpdated",
    "CurveStableswapPool",
    "CurveStableswapPoolSimulationResult",
    "CurveStableswapPoolState",
    "EncodedCall",
    "Erc20Token",
    "EtherPlaceholder",
    "FlatComposer",
    "ManagedPoolRegistry",
    "NoApprovals",
    "PancakeswapV2Pool",
    "PancakeswapV2PoolTracker",
    "PancakeswapV3Pool",
    "PancakeswapV3PoolTracker",
    "PayloadComposer",
    "PoolRegistry",
    "PoolTypeRegistry",
    "SushiswapV2Pool",
    "SushiswapV2PoolTracker",
    "SushiswapV3Pool",
    "SushiswapV3PoolTracker",
    "SwapbasedV2Pool",
    "SwapbasedV2PoolTracker",
    "TokenRegistry",
    "UniswapV2Pool",
    "UniswapV2PoolExternalUpdate",
    "UniswapV2PoolSimulationResult",
    "UniswapV2PoolState",
    "UniswapV2PoolTracker",
    "UniswapV3LiquiditySnapshot",
    "UniswapV3Pool",
    "UniswapV3PoolExternalUpdate",
    "UniswapV3PoolSimulationResult",
    "UniswapV3PoolState",
    "UniswapV3PoolTracker",
    "UniswapV4LiquiditySnapshot",
    "UniswapV4Pool",
    "UniswapV4PoolExternalUpdate",
    "UniswapV4PoolState",
    "V4PoolKey",
    "__version__",
    "abi_decode",
    "abi_decode_single",
    "abi_encode",
    "decode_return_data",
    "encode_function_call",
    "generate_payloads",
    "get_checksum_address",
    "get_default_adapter",
    "get_default_backend",
    "get_function_selector",
    "get_sqrt_ratio_at_tick",
    "get_tick_at_sqrt_ratio",
    "logger",
    "pool_type_registry",
    "to_checksum_address",
)


_DEPRECATED_NAMES = {
    "UniswapLpCycle": "degenbot.arbitrage._legacy._uniswap_lp_cycle:_UniswapLpCycle",
    "UniswapCurveCycle": "degenbot.arbitrage._legacy._uniswap_curve_cycle:_UniswapCurveCycle",
}


def __getattr__(name: str) -> object:
    if name in _DEPRECATED_NAMES:
        warnings.warn(
            f"{name} is deprecated. Use ArbitragePath + ArbSolver instead. "
            "See docs/migration-guides/legacy-cycles-to-arbitrage-path.md",
            DeprecationWarning,
            stacklevel=2,
        )
        module_path, attr = _DEPRECATED_NAMES[name].rsplit(":", 1)
        return getattr(importlib.import_module(module_path), attr)
    msg = f"module {__name__!r} has no attribute {name!r}"
    raise AttributeError(msg)
