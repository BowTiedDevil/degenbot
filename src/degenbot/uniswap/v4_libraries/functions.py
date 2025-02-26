from degenbot.exceptions import EVMRevertError


def mulmod(x: int, y: int, k: int) -> int:
    if k == 0:
        raise EVMRevertError(error="division by zero")
    return (x * y) % k
