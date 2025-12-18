from degenbot.functions import raise_if_invalid_uint256
from degenbot.solidly.solidly_functions import general_calc_d


def f_camelot(x0: int, y: int) -> int:
    return (
        x0 * (y * y // 10**18 * y // 10**18) // 10**18
        + (x0 * x0 // 10**18 * x0 // 10**18) * y // 10**18
    )


def get_y_camelot(
    x_0: int,
    xy: int,
    y: int,
) -> int:  # pragma: no cover
    for _ in range(255):
        y_prev = y
        k = f_camelot(x_0, y)
        if k < xy:
            dy = (xy - k) * 10**18 // general_calc_d(x_0, y)
            y += dy
        else:
            dy = (k - xy) * 10**18 // general_calc_d(x_0, y)
            y -= dy

        if y > y_prev:
            if y - y_prev <= 1:
                return y
        elif y_prev - y <= 1:
            return y
    return y


def k_camelot(
    balance_0: int,
    balance_1: int,
    decimals_0: int,
    decimals_1: int,
) -> int:
    x = balance_0 * 10**18 // decimals_0
    y = balance_1 * 10**18 // decimals_1
    a = (x * y) // 10**18
    b = (x * x) // 10**18 + (y * y) // 10**18
    raise_if_invalid_uint256(a * b)
    return a * b // 10**18  # x^3*y + y^3*x >= k
