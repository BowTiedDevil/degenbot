from functools import lru_cache


@lru_cache
def div_rounding_up(x: int, y: int) -> int:
    """
    Perform an x//y floored division, rounding up any remainder.
    """
    return x // y + (x % y != 0)
