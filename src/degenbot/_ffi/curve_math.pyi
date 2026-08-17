def stableswap_get_d(
    xp: list[int],
    amp: int,
    n_coins: int,
    a_precision: int,
    d_variant: int,
) -> int: ...
def stableswap_get_y(
    i: int,
    j: int,
    x: int,
    xp: list[int],
    amp: int,
    n_coins: int,
    a_precision: int,
    y_variant: int,
    d_variant: int,
) -> int: ...
def stableswap_get_y_d(
    amp: int,
    i: int,
    xp: list[int],
    d: int,
    n_coins: int,
    a_precision: int,
    yd_variant: int,
) -> int: ...
def stableswap_newton_y(
    ann: int,
    gamma: int,
    xp: list[int],
    d: int,
    token_index: int,
    n_coins: int,
    a_multiplier: int,
) -> int: ...
def stableswap_reduction_coefficient(x: list[int], fee_gamma: int, n_coins: int) -> int: ...

__all__ = [
    "stableswap_get_d",
    "stableswap_get_y",
    "stableswap_get_y_d",
    "stableswap_newton_y",
    "stableswap_reduction_coefficient",
]
