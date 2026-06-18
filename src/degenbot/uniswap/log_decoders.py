"""Log decoders for Uniswap pool events.

Each decoder takes a raw log dict (as produced by the Rust subscription
layer) and returns a closure that applies the decoded update to the
pool instance when called.

Log dict format (from ``log_to_py_dict`` in ``py_converters.rs``):
- ``address``: checksummed str
- ``topics``: list[HexBytes]
- ``data``: HexBytes
- ``blockNumber``: int | None
- ``logIndex``: int | None

ABI decoding uses ``eth_abi.decode()`` on the data field.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from eth_abi import decode as abi_decode
from hexbytes import HexBytes

from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate
from degenbot.uniswap.v3_types import (
    UniswapV3PoolExternalUpdate,
    UniswapV3PoolLiquidityMappingUpdate,
)
from degenbot.uniswap.v4_types import (
    UniswapV4PoolExternalUpdate,
    UniswapV4PoolLiquidityMappingUpdate,
)

if TYPE_CHECKING:
    from collections.abc import Callable

    from degenbot.uniswap.liquidity_pool import LiquidityPool
    from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
    from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

# ---- Event topic hashes (keccak256 of event signatures) ----

V2_SYNC_TOPIC = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"
V3_SWAP_TOPIC = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"
V3_MINT_TOPIC = "0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde"
V3_BURN_TOPIC = "0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c"
V4_SWAP_TOPIC = "0x40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f"
V4_MODIFY_LIQUIDITY_TOPIC = "0xf208f4912782fd25c7f114ca3723a2d5dd6f3bcc3ac8db5af63baa85f711d5ec"


def get_block_number(log: dict) -> int:
    """Extract block number from a log dict.

    Returns:
        The block number as an integer.

    Raises:
        ValueError: If the log dict is missing blockNumber.

    """
    block_number = log.get("blockNumber")
    if block_number is None:
        msg = "Log dict missing blockNumber"
        raise ValueError(msg)
    return int(block_number)


def get_log_data_bytes(log: dict) -> bytes:
    """Extract the data field from a log dict as bytes.

    Returns:
        The data field as raw bytes.

    Raises:
        ValueError: If the log dict is missing data.

    """
    data = log.get("data")
    if data is None:
        msg = "Log dict missing data"
        raise ValueError(msg)
    return bytes(HexBytes(data))


def get_topic_bytes(log: dict, index: int) -> bytes:
    """Extract a topic from a log dict as bytes.

    Returns:
        The topic at the given index as raw bytes.

    Raises:
        ValueError: If the log dict is missing the requested topic.

    """
    topics = log.get("topics")
    if topics is None or index >= len(topics):
        msg = f"Log dict missing topic at index {index}"
        raise ValueError(msg)
    return bytes(HexBytes(topics[index]))


# ---- V2 Decoders ----


def decode_v2_sync(log: dict) -> Callable[[LiquidityPool], None]:
    """Decode a V2 Sync event and return a closure that applies the update.

    Sync(uint112,uint112) — data is tightly packed reserve0, reserve1.

    Returns:
        A callable that applies the decoded update to a V2 pool.

    """
    reserve0, reserve1 = abi_decode(
        ["uint112", "uint112"],
        get_log_data_bytes(log),
    )
    block_number = get_block_number(log)

    update = UniswapV2PoolExternalUpdate(
        block_number=block_number,
        reserves_token0=reserve0,
        reserves_token1=reserve1,
    )

    def apply(pool: LiquidityPool) -> None:
        pool.external_update(update)

    return apply


# ---- V3 Decoders ----


def decode_v3_swap(log: dict) -> Callable[[UniswapV3Pool], None]:
    """Decode a V3 Swap event and return a closure that applies the update.

    Swap(address,address,int256,int256,uint160,uint128,int24)
    Data: amount0, amount1, sqrtPriceX96, liquidity, tick.

    Returns:
        A callable that applies the decoded update to a V3 pool.

    """
    _amount0, _amount1, sqrt_price_x96, liquidity, tick = abi_decode(
        ["int256", "int256", "uint160", "uint128", "int24"],
        get_log_data_bytes(log),
    )
    block_number = get_block_number(log)

    update = UniswapV3PoolExternalUpdate(
        block_number=block_number,
        liquidity=liquidity,
        sqrt_price_x96=sqrt_price_x96,
        tick=tick,
    )

    def apply(pool: UniswapV3Pool) -> None:
        pool.external_update(update)

    return apply


def decode_v3_mint(log: dict) -> Callable[[UniswapV3Pool], None]:
    """Decode a V3 Mint event and return a closure for the liquidity update.

    Mint(address,address,int24,int24,uint128,uint256,uint256)
    Data: tickLower, tickUpper, amount, amount0, amount1.

    Returns:
        A callable that applies the decoded update to a V3 pool.

    """
    tick_lower, tick_upper, amount, _amount0, _amount1 = abi_decode(
        ["int24", "int24", "uint128", "uint256", "uint256"],
        get_log_data_bytes(log),
    )
    block_number = get_block_number(log)

    update = UniswapV3PoolLiquidityMappingUpdate(
        block_number=block_number,
        liquidity=amount,
        tick_lower=tick_lower,
        tick_upper=tick_upper,
    )

    def apply(pool: UniswapV3Pool) -> None:
        pool.update_liquidity_map(update)

    return apply


def decode_v3_burn(log: dict) -> Callable[[UniswapV3Pool], None]:
    """Decode a V3 Burn event and return a closure for the liquidity update.

    Burn(address,int24,int24,uint128,uint256,uint256)
    Data: tickLower, tickUpper, amount, amount0, amount1.

    Returns:
        A callable that applies the decoded update to a V3 pool.

    """
    tick_lower, tick_upper, amount, _amount0, _amount1 = abi_decode(
        ["int24", "int24", "uint128", "uint256", "uint256"],
        get_log_data_bytes(log),
    )
    block_number = get_block_number(log)

    update = UniswapV3PoolLiquidityMappingUpdate(
        block_number=block_number,
        liquidity=-amount,  # Burn decreases liquidity
        tick_lower=tick_lower,
        tick_upper=tick_upper,
    )

    def apply(pool: UniswapV3Pool) -> None:
        pool.update_liquidity_map(update)

    return apply


# ---- V4 Decoders ----


def _extract_v4_pool_id_from_topics(log: dict) -> bytes:
    """Extract the V4 pool ID from topic[1] of a V4 event.

    Returns:
        The pool ID as raw bytes.

    """
    return get_topic_bytes(log, 1)


def decode_v4_swap(log: dict) -> Callable[[UniswapV4Pool], None]:
    """Decode a V4 Swap event and return a closure that applies the update.

    Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)
    Data: amount0, amount1, sqrtPriceX96, liquidity, tick, fee.

    Returns:
        A callable that applies the decoded update to a V4 pool.

    """
    _amount0, _amount1, sqrt_price_x96, liquidity, tick, _fee = abi_decode(
        ["int128", "int128", "uint160", "uint128", "int24", "uint24"],
        get_log_data_bytes(log),
    )
    block_number = get_block_number(log)

    update = UniswapV4PoolExternalUpdate(
        block_number=block_number,
        liquidity=liquidity,
        sqrt_price_x96=sqrt_price_x96,
        tick=tick,
    )

    def apply(pool: UniswapV4Pool) -> None:
        pool.external_update(update)

    return apply


def decode_v4_modify_liquidity(log: dict) -> Callable[[UniswapV4Pool], None]:
    """Decode a V4 ModifyLiquidity event and return a closure for the update.

    ModifyLiquidity(bytes32,address,int24,int24,int256,bytes32)
    Data: tickLower, tickUpper, liquidityDelta, salt.

    Returns:
        A callable that applies the decoded update to a V4 pool.

    """
    tick_lower, tick_upper, liquidity_delta, _salt = abi_decode(
        ["int24", "int24", "int256", "bytes32"],
        get_log_data_bytes(log),
    )
    block_number = get_block_number(log)

    update = UniswapV4PoolLiquidityMappingUpdate(
        block_number=block_number,
        liquidity=liquidity_delta,
        tick_lower=tick_lower,
        tick_upper=tick_upper,
    )

    def apply(pool: UniswapV4Pool) -> None:
        pool.update_liquidity_map(update)

    return apply
