from degenbot.constants import MAX_UINT256
from degenbot.exceptions.evm import EVMRevertError

# Percentage: decimal numbers with 4 digits of precision (100.00%)
PERCENTAGE_FACTOR = 10**4
HALF_PERCENTAGE_FACTOR = 5 * 10**3


def percent_mul(value: int, percentage: int) -> int:
    if percentage != 0 and value > (MAX_UINT256 - HALF_PERCENTAGE_FACTOR) // percentage:
        raise EVMRevertError
    return (value * percentage + HALF_PERCENTAGE_FACTOR) // PERCENTAGE_FACTOR


def percent_div(value: int, percentage: int) -> int:
    if percentage == 0:
        raise EVMRevertError
    if value > (MAX_UINT256 - (percentage // 2)) // PERCENTAGE_FACTOR:
        raise EVMRevertError
    return (value * PERCENTAGE_FACTOR + (percentage // 2)) // percentage


def percent_mul_floor(value: int, percentage: int) -> int:
    if percentage != 0 and value > MAX_UINT256 // percentage:
        raise EVMRevertError
    return (value * percentage) // PERCENTAGE_FACTOR


def percent_mul_ceil(value: int, percentage: int) -> int:
    if percentage != 0 and value > MAX_UINT256 // percentage:
        raise EVMRevertError
    product = value * percentage
    return (product // PERCENTAGE_FACTOR) + (1 if product % PERCENTAGE_FACTOR != 0 else 0)


def percent_div_ceil(value: int, percentage: int) -> int:
    if percentage == 0:
        raise EVMRevertError
    if value > MAX_UINT256 // PERCENTAGE_FACTOR:
        raise EVMRevertError
    val = value * PERCENTAGE_FACTOR
    return (val // percentage) + (1 if val % percentage != 0 else 0)
