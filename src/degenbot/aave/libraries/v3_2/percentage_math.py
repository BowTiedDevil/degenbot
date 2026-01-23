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
