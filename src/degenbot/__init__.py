"""degenbot: Ethereum DEX helper library."""

from .abi import (
    AbiDecodeError,
    AbiEncodeError,
)
from .abi import decode as abi_decode
from .abi import decode_single as abi_decode_single
from .abi import encode as abi_encode
from .bot import Bot
from .checksum_cache import get_checksum_address
from .version import __version__

# isort: split

from . import (
    camelot as camelot,
)
from .aerodrome import (
    AerodromeV2Pool,
    AerodromeV2PoolState,
    AerodromeV2PoolTracker,
    AerodromeV3Pool,
    AerodromeV3PoolState,
    AerodromeV3PoolTracker,
)
from .chainlink import ChainlinkPriceContract
from .curve import (
    CurveStableswapPool,
    CurveStableswapPoolSimulationResult,
    CurveStableswapPoolState,
    CurveStableSwapPoolStateUpdated,
)
from .erc20 import Erc20Token, EtherPlaceholder
from .fork import AnvilFork
from .logging import logger
from .pancakeswap import (
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

# Populate the pool-type registry from the shipped deployments JSON
# (single source of DEX deployment data — ADR-005). This replaces the
# per-module `_register_*_deployments()` inline tuples that previously
# lived in each DEX `__init__.py`. Runs once after all DEX classes are
# imported. A user overlay may be declared in ``[deployments]`` in
# ``~/.config/degenbot/config.toml``.
from .registry.deployment_loader import (
    load_deployments as _load_deployments,
)
from .registry.deployment_loader import (
    register_from_deployments as _register_from_deployments,
)
from .sushiswap import (
    SushiswapV3PoolTracker,
)
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

_register_from_deployments(_load_deployments(), pool_type_registry)

__all__ = (
    "AbiDecodeError",
    "AbiEncodeError",
    "AerodromeV2Pool",
    "AerodromeV2PoolState",
    "AerodromeV2PoolTracker",
    "AerodromeV3Pool",
    "AerodromeV3PoolState",
    "AerodromeV3PoolTracker",
    "AnvilFork",
    "Bot",
    "ChainlinkPriceContract",
    "CurveStableSwapPoolStateUpdated",
    "CurveStableswapPool",
    "CurveStableswapPoolSimulationResult",
    "CurveStableswapPoolState",
    "Erc20Token",
    "EtherPlaceholder",
    "ManagedPoolRegistry",
    "PancakeswapV3Pool",
    "PancakeswapV3PoolTracker",
    "PoolRegistry",
    "PoolTypeRegistry",
    "SushiswapV3PoolTracker",
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
    "__version__",
    "abi_decode",
    "abi_decode_single",
    "abi_encode",
    "get_checksum_address",
    "logger",
    "pool_type_registry",
)
