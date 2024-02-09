from . import yul_operations as yul


def divRoundingUp(x: int, y: int) -> int:
    return yul.add(yul.div(x, y), yul.gt(yul.mod(x, y), 0))
