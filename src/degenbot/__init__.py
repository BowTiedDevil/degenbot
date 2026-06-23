"""degenbot: Ethereum DEX helper library."""

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

from . import (
    camelot as camelot,
)
from . import (
    swapbased as swapbased,
)
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
    SushiswapV3Pool,
    SushiswapV3PoolTracker,
)
from .uniswap import (
    LiquidityPool,
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
    "Bot",
    "ChainlinkPriceContract",
    "CurveStableSwapPoolStateUpdated",
    "CurveStableswapPool",
    "CurveStableswapPoolSimulationResult",
    "CurveStableswapPoolState",
    "EncodedCall",
    "Erc20Token",
    "EtherPlaceholder",
    "FlatComposer",
    "LiquidityPool",
    "ManagedPoolRegistry",
    "NoApprovals",
    "PancakeswapV3Pool",
    "PancakeswapV3PoolTracker",
    "PayloadComposer",
    "PoolRegistry",
    "PoolTypeRegistry",
    "SushiswapV3Pool",
    "SushiswapV3PoolTracker",
    "TokenRegistry",
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
