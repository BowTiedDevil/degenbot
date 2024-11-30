from functools import lru_cache

from degenbot.uniswap.v3_libraries._config import LRU_CACHE_SIZE


@lru_cache(maxsize=LRU_CACHE_SIZE)
def div_rounding_up(x: int, y: int) -> int:
    """
    Perform an x//y floored division, rounding up any remainder.
    """
    return x // y + (x % y != 0)
