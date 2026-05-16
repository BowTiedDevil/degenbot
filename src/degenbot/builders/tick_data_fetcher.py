from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING, Any, cast

import eth_abi.abi

from degenbot.checksum_cache import get_checksum_address
from degenbot.provider.call_helpers import encode_function_calldata, raw_call
from degenbot.provider.interface import ProviderAdapter

if TYPE_CHECKING:
    from collections.abc import Callable

    from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
    from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool


@dataclasses.dataclass(slots=True, frozen=True)
class TickDataTypes:
    """
    Type-params that differ between V3 and V4 tick data fetchers.

    V3 and V4 use the same algorithm but different concrete types for
    the bitmap-at-word and liquidity-at-tick values. This dataclass
    captures those differences so the algorithm can be written once.
    """

    bitmap_at_word: type  # UniswapV3BitmapAtWord or UniswapV4BitmapAtWord
    liquidity_at_tick: type  # UniswapV3LiquidityAtTick or UniswapV4LiquidityAtTick
    tick_struct_types: tuple[str, ...]  # ABI types for decoding tick data


def make_tick_data_fetcher(
    pool_lookup: Callable[[int], UniswapV3Pool | UniswapV4Pool | None],
    provider_lookup: Callable[[], ProviderAdapter],
    types: TickDataTypes,
    *,
    state_view_address: str | None = None,
    pool_id: bytes | None = None,
) -> Callable[[int, int], None]:
    """
    Create a tick data fetcher callback for a concentrated-liquidity pool.

    The returned fetcher captures the pool-lookup and provider-lookup
    closures so the calling builder does not need to hold references
    to registries or connection managers.

    For V4 pools, pass ``state_view_address`` and ``pool_id`` so the
    fetcher calls the state-view contract with the correct V4 ABI
    (``getTickBitmap(bytes32,int16)`` / ``getTickLiquidity(bytes32,int24)``).
    When these are absent the fetcher uses V3 ABI calls
    (``tickBitmap(int16)`` / ``ticks(int24)``) on ``pool.address``.
    """

    is_v4 = state_view_address is not None and pool_id is not None

    def fetcher(word_position: int, block_number: int) -> None:
        pool = pool_lookup(block_number)
        if pool is None:
            return

        provider = provider_lookup()
        working_tick_bitmap = dict(pool.tick_bitmap)
        working_tick_data = dict(pool.tick_data)

        if isinstance(pool, UniswapV4Pool):
            _fetch_v4(
                provider=provider,
                state_view_address=cast("str", state_view_address),
                pool_id=cast("bytes", pool_id),
                word_position=word_position,
                block_number=block_number,
                pool=pool,
                working_tick_bitmap=working_tick_bitmap,
                working_tick_data=working_tick_data,
                types=types,
            )
        else:
            _fetch_v3(
                provider=provider,
                pool=pool,
                word_position=word_position,
                block_number=block_number,
                working_tick_bitmap=working_tick_bitmap,
                working_tick_data=working_tick_data,
                types=types,
            )

        new_state = dataclasses.replace(
            pool.state,
            tick_bitmap=working_tick_bitmap,  # type: ignore[arg-type]
            tick_data=working_tick_data,  # type: ignore[arg-type]
            block=max(pool.update_block, block_number),
        )
        pool._state_mgr.push_state(new_state)  # type: ignore[arg-type]  # noqa: SLF001

    return fetcher


def _fetch_v3(
    *,
    provider: ProviderAdapter,
    pool: UniswapV3Pool,
    word_position: int,
    block_number: int,
    working_tick_bitmap: dict[int, Any],
    working_tick_data: dict[int, Any],
    types: TickDataTypes,
) -> None:
    """Fetch tick bitmap + data for a V3 pool using its direct contract calls."""
    try:
        (bitmap_value,) = raw_call(
            provider,
            address=pool.address,
            calldata=encode_function_calldata("tickBitmap(int16)", [word_position]),
            return_types=["uint256"],
            block_identifier=block_number,
        )
    except Exception:  # noqa: BLE001
        return

    working_tick_bitmap[word_position] = types.bitmap_at_word(
        bitmap=bitmap_value, block=block_number
    )

    if bitmap_value != 0:
        active_ticks = [
            ((word_position << 8) + i) * pool.tick_spacing
            for i in range(256)
            if bitmap_value & (1 << i) > 0
        ]

        for active_tick in active_ticks:
            try:
                result = provider.call(
                    to=pool.address,
                    data=encode_function_calldata("ticks(int24)", [active_tick]),
                    block=block_number,
                )
            except Exception:  # noqa: BLE001
                continue

            liquidity_gross, liquidity_net, *_ = eth_abi.abi.decode(
                types=types.tick_struct_types,
                data=result,
            )
            working_tick_data[active_tick] = types.liquidity_at_tick(
                liquidity_net=int(liquidity_net),
                liquidity_gross=int(liquidity_gross),
                block=block_number,
            )


def _fetch_v4(
    *,
    provider: ProviderAdapter,
    state_view_address: str,
    pool_id: bytes,
    word_position: int,
    block_number: int,
    pool: UniswapV4Pool,
    working_tick_bitmap: dict[int, Any],
    working_tick_data: dict[int, Any],
    types: TickDataTypes,
) -> None:
    """Fetch tick bitmap + data for a V4 pool via the state-view contract."""
    try:
        (bitmap_value,) = raw_call(
            provider,
            address=get_checksum_address(state_view_address),
            calldata=encode_function_calldata(
                "getTickBitmap(bytes32,int16)", [pool_id, word_position]
            ),
            return_types=["uint256"],
            block_identifier=block_number,
        )
    except Exception:  # noqa: BLE001
        return

    working_tick_bitmap[word_position] = types.bitmap_at_word(
        bitmap=bitmap_value, block=block_number
    )

    if bitmap_value != 0:
        active_ticks = [
            ((word_position << 8) + i) * pool.tick_spacing
            for i in range(256)
            if bitmap_value & (1 << i) > 0
        ]

        for active_tick in active_ticks:
            try:
                result = provider.call(
                    to=state_view_address,
                    data=encode_function_calldata(
                        "getTickLiquidity(bytes32,int24)",
                        [pool_id, active_tick],
                    ),
                    block=block_number,
                )
            except Exception:  # noqa: BLE001
                continue

            liquidity_gross, liquidity_net = eth_abi.abi.decode(
                types=types.tick_struct_types,
                data=result,
            )
            working_tick_data[active_tick] = types.liquidity_at_tick(
                liquidity_net=int(liquidity_net),
                liquidity_gross=int(liquidity_gross),
                block=block_number,
            )
