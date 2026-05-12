from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING, Any

import eth_abi.abi

from degenbot.functions import encode_function_calldata, raw_call

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
    pool_lookup: Callable[[int], UniswapV3Pool | UniswapV4Pool | None],  # type: ignore[valid-type]
    provider_lookup: Callable[[], Any],
    types: TickDataTypes,
) -> Callable[[int, int], None]:
    """
    Create a tick data fetcher callback for a concentrated-liquidity pool.

    The returned fetcher captures the pool-lookup and provider-lookup
    closures so the calling builder does not need to hold references
    to registries or connection managers.
    """

    def fetcher(word_position: int, block_number: int) -> None:
        pool = pool_lookup(block_number)
        if pool is None:
            return

        provider = provider_lookup()
        working_tick_bitmap = dict(pool.tick_bitmap)
        working_tick_data = dict(pool.tick_data)

        try:
            (bitmap_value,) = raw_call(
                provider,
                address=pool.address,
                calldata=encode_function_calldata(
                    "tickBitmap(int16)", [word_position]
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
                        to=pool.address,
                        data=encode_function_calldata(
                            "ticks(int24)", [active_tick]
                        ),
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

        new_state = dataclasses.replace(
            pool.state,
            tick_bitmap=working_tick_bitmap,
            tick_data=working_tick_data,
            block=max(pool.update_block, block_number),
        )
        pool._state_mgr.push_state(new_state)  # noqa: SLF001

    return fetcher
